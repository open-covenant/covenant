use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use covenant_compute::{
    AppCatalog, ComputeOffer, ComputeReceipt, GpuSpec, JobStatus, LaunchPlan, TrustClass,
    MIN_DURATION_SECS,
};
use tempfile::TempDir;
use tokio::sync::{Mutex, Notify};
use tower::ServiceExt;

use crate::{
    router, AuthRegistry, BetaCredential, ControlPlane, JobClock, PlanRejection, Principal,
    ProviderBackend, ProviderCancel, ProviderError, ProviderJob, ProviderLaunch, ProviderPoll,
    ServiceError, SqliteStore, StoreError,
};

const TEST_TOKEN: &str = "test-token-long-enough";

struct FakeProvider {
    offers: Vec<ComputeOffer>,
    available: AtomicBool,
    launch_available: AtomicBool,
    launch_rejected: AtomicBool,
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
            launch_rejected: AtomicBool::new(false),
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

    fn set_available(&self, available: bool) {
        self.available.store(available, Ordering::SeqCst);
    }

    fn set_launch_available(&self, available: bool) {
        self.launch_available.store(available, Ordering::SeqCst);
    }

    fn set_launch_rejected(&self, rejected: bool) {
        self.launch_rejected.store(rejected, Ordering::SeqCst);
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
        if self.launch_rejected.load(Ordering::SeqCst) {
            return Err(ProviderError::Rejected);
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
        *self.last_cancel_window.lock().await = Some((
            request.clock.billed_from_ms(),
            request.clock.requested_at_ms,
        ));
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
                            provisioning_secs: 0,
                            provisioning_usdc_micros: 0,
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
                provisioning_secs: 0,
                provisioning_usdc_micros: 0,
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

fn clock(created_at_ms: u64, ready_at_ms: Option<u64>, requested_at_ms: u64) -> JobClock {
    JobClock {
        created_at_ms,
        ready_at_ms,
        requested_at_ms,
    }
}

/// The lifecycle log is the only record an operator has of a disputed charge,
/// so it is asserted rather than eyeballed. One global subscriber serves every
/// test; each one selects its own job id out of the shared buffer.
#[derive(Clone, Default)]
struct CapturedLogs(Arc<std::sync::Mutex<Vec<u8>>>);

impl CapturedLogs {
    fn lines_for(&self, job_id: &str) -> Vec<String> {
        String::from_utf8_lossy(&self.0.lock().unwrap())
            .lines()
            .filter(|line| line.contains(job_id))
            .map(str::to_owned)
            .collect()
    }
}

impl std::io::Write for CapturedLogs {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl tracing_subscriber::fmt::MakeWriter<'_> for CapturedLogs {
    type Writer = Self;

    fn make_writer(&self) -> Self::Writer {
        self.clone()
    }
}

fn captured_logs() -> &'static CapturedLogs {
    static LOGS: std::sync::OnceLock<CapturedLogs> = std::sync::OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = CapturedLogs::default();
        let _ = tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .with_ansi(false)
                .with_writer(logs.clone())
                .finish(),
        );
        logs
    })
}

fn test_router(control: ControlPlane) -> axum::Router {
    let auth = Arc::new(
        AuthRegistry::new(vec![BetaCredential {
            owner: "owner-a".into(),
            token: TEST_TOKEN.into(),
            spend_cap_usdc_micros: 1_000_000,
        }])
        .unwrap(),
    );
    router(auth, control)
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
    assert_eq!(report.reconciled, 1);
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
    let app = test_router(control);

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
async fn malformed_bodies_are_authenticated_first_and_answered_in_the_error_envelope() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let control = control_plane(&temp, provider).await.unwrap();
    let app = test_router(control);

    let unauthenticated = app
        .clone()
        .oneshot(
            Request::post("/v1/jobs")
                .header("content-type", "application/json")
                .header("idempotency-key", "envelope")
                .body(Body::from("{not json"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let malformed = app
        .clone()
        .oneshot(
            Request::post("/v1/jobs")
                .header("content-type", "application/json")
                .header("authorization", format!("bearer {TEST_TOKEN}"))
                .header("idempotency-key", "envelope")
                .body(Body::from("{not json"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        malformed
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "application/json"
    );
    let body = axum::body::to_bytes(malformed.into_body(), 4_096)
        .await
        .unwrap();
    let envelope: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(envelope["error"]["code"], "malformed_json");
    assert_eq!(
        envelope["error"]["message"],
        "the request body is not valid JSON"
    );
}

#[tokio::test]
async fn a_well_formed_body_of_the_wrong_shape_names_the_field_at_fault() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let control = control_plane(&temp, provider).await.unwrap();
    let app = test_router(control);

    let mut wrong_type = serde_json::to_value(plan(1_800)).unwrap();
    wrong_type["duration_secs"] = serde_json::json!("ten");
    let cases = [
        (
            serde_json::json!({"hello": "world"}),
            "the request body is missing the field `app`",
        ),
        (
            serde_json::json!({"app": {}, "offer": {}}),
            "the request body is missing the field `app.id`",
        ),
        (
            wrong_type,
            "the request body field `duration_secs` is not a valid launch plan value",
        ),
    ];

    for (body, message) in cases {
        let response = app
            .clone()
            .oneshot(
                Request::post("/v1/jobs")
                    .header("content-type", "application/json")
                    .header("authorization", format!("bearer {TEST_TOKEN}"))
                    .header("idempotency-key", "wrong-shape")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), 4_096)
            .await
            .unwrap();
        let envelope: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(envelope["error"]["code"], "invalid_request_body");
        assert_eq!(envelope["error"]["message"], message);
    }
}

#[tokio::test]
async fn a_body_sent_without_the_json_content_type_says_so() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let control = control_plane(&temp, provider).await.unwrap();
    let app = test_router(control);

    let response = app
        .oneshot(
            Request::post("/v1/jobs")
                .header("authorization", format!("bearer {TEST_TOKEN}"))
                .header("idempotency-key", "no-content-type")
                .body(Body::from(serde_json::to_vec(&plan(1_800)).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), 4_096)
        .await
        .unwrap();
    let envelope: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(envelope["error"]["code"], "invalid_content_type");
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
    let logs = captured_logs();
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
            clock: clock(0, None, 1),
        })
        .await
        .unwrap();
    store
        .record_provider(&submitted.job.id, provider_job, 1)
        .await
        .unwrap();

    let control = ControlPlane::new(AppCatalog::builtin(), store, provider);
    let report = control.recover().await.unwrap();
    assert_eq!(report.reconciled, 1);
    let job = control
        .job(&principal("owner-a", 1_000_000), &submitted.job.id)
        .await
        .unwrap();
    assert_eq!(job.status, JobStatus::Cancelled);
    assert!(job.receipt.is_some());
    assert!(logs
        .lines_for(&submitted.job.id)
        .iter()
        .any(|line| line.contains("compute job reached its deadline")));
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
                    provisioning_secs: 0,
                    provisioning_usdc_micros: 0,
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
async fn an_allocation_left_by_a_failed_teardown_is_swept_and_swept_once() {
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
        async move { control.submit(&owner, "orphan-sweep", launch_plan).await }
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
                    provisioning_secs: 0,
                    provisioning_usdc_micros: 0,
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

    // The late launch lands while the provider cannot be reached, so its
    // teardown fails and the machine is left running.
    provider.set_available(false);
    provider.release_blocked_launch();
    assert_eq!(submit.await.unwrap().unwrap().status, JobStatus::Cancelled);
    assert_eq!(provider.active_allocations.load(Ordering::SeqCst), 1);

    provider.set_available(true);
    let report = control.recover().await.unwrap();
    assert_eq!(report.released, 1);
    assert_eq!(provider.active_allocations.load(Ordering::SeqCst), 0);
    assert_eq!(control.recover().await.unwrap().released, 0);
}

#[tokio::test]
async fn a_launch_rejected_after_a_cancel_settles_instead_of_failing() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    provider.block_next_launch();
    provider.set_launch_rejected(true);
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
                .submit(&owner, "reject-after-cancel", plan(1_800))
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

    provider.release_blocked_launch();
    assert_eq!(submit.await.unwrap().unwrap().status, JobStatus::Failed);
    control
        .submit(&owner, "after-rejected-race", plan(1_800))
        .await
        .expect("the reservation was released");
}

#[tokio::test]
async fn a_job_that_never_provisions_is_cancelled_with_a_full_refund() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    provider.set_launch_provisioning(true);
    let store = SqliteStore::open(temp.path().join("control.sqlite3"))
        .await
        .unwrap();
    let owner = principal("owner-a", 10_000_000);
    let launch_plan = plan(21_600);
    let submitted = store
        .submit(
            &owner.id,
            owner.spend_cap_usdc_micros,
            "stuck",
            &launch_plan,
            0,
        )
        .await
        .unwrap();
    let provider_job = provider
        .launch(ProviderLaunch {
            job_id: submitted.job.id.clone(),
            idempotency_key: "stuck".into(),
            plan: launch_plan.clone(),
            clock: clock(0, None, 1),
        })
        .await
        .unwrap();
    store
        .record_provider(&submitted.job.id, provider_job, 1)
        .await
        .unwrap();

    // Still provisioning long past the provisioning timeout, while the rental
    // itself has hours left on the clock.
    let control = ControlPlane::new(AppCatalog::builtin(), store, provider);
    assert_eq!(control.recover().await.unwrap().reconciled, 1);
    let job = control.job(&owner, &submitted.job.id).await.unwrap();
    assert_eq!(job.status, JobStatus::Cancelled);
    let receipt = job.receipt.unwrap();
    assert_eq!(receipt.charged_usdc_micros, 0);
    assert_eq!(
        receipt.refunded_usdc_micros,
        launch_plan.maximum_usdc_micros
    );
}

#[tokio::test]
async fn a_raised_spend_cap_is_adopted_and_a_lowered_one_respects_commitments() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let control = control_plane(&temp, provider).await.unwrap();
    control
        .submit(&principal("owner-a", 400_000), "cap-first", plan(1_800))
        .await
        .unwrap();

    control
        .submit(&principal("owner-a", 900_000), "cap-raised", plan(1_800))
        .await
        .expect("a raised cap is adopted");

    let error = control
        .submit(&principal("owner-a", 100_000), "cap-lowered", plan(1_800))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ServiceError::Store(StoreError::SpendCapBelowCommitments)
    ));
}

#[tokio::test]
async fn the_job_list_is_bounded_and_skips_unreadable_rows() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("control.sqlite3");
    let store = SqliteStore::open(&path).await.unwrap();
    let launch_plan = plan(60);
    for index in 0..105 {
        store
            .submit(
                "owner-a",
                100_000_000,
                &format!("bulk-{index}"),
                &launch_plan,
                u64::try_from(index).unwrap(),
            )
            .await
            .unwrap();
    }
    assert_eq!(store.jobs("owner-a").await.unwrap().len(), 100);

    let newest = store.jobs("owner-a").await.unwrap().remove(0);
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE jobs SET plan_json = 'not-json' WHERE id = ?1",
            rusqlite::params![newest.id],
        )
        .unwrap();
    assert_eq!(store.jobs("owner-a").await.unwrap().len(), 99);
    assert_eq!(store.recoverable_jobs().await.unwrap().len(), 104);
}

#[tokio::test]
async fn one_malformed_provider_offer_does_not_take_down_the_catalog() {
    let temp = TempDir::new().unwrap();
    let malformed = ComputeOffer {
        id: "offer-b".into(),
        rate_usdc_micros_per_hour: 0,
        ..offer()
    };
    let provider = Arc::new(FakeProvider::new(vec![offer(), malformed.clone()]));
    let control = control_plane(&temp, provider).await.unwrap();
    let offers = control.offers().await.unwrap();
    assert_eq!(offers, vec![offer()]);

    let provider = Arc::new(FakeProvider::new(vec![malformed]));
    let control = control_plane(&temp, provider).await.unwrap();
    assert!(matches!(
        control.offers().await.unwrap_err(),
        ServiceError::InvalidProviderOffers
    ));
}

#[tokio::test]
async fn launch_guards_do_not_accumulate_per_job() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let control = control_plane(&temp, provider).await.unwrap();
    let owner = principal("owner-a", 10_000_000);
    for index in 0..8 {
        control
            .submit(&owner, &format!("guard-{index}"), plan(1_800))
            .await
            .unwrap();
    }
    assert!(control.launch_guards.lock().await.len() <= 1);
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
                clock: clock(submitted.job.created_at_ms, None, now_ms),
            })
            .await
            .unwrap();
        // Both jobs went running when they were created, so the expired one is
        // already past its billed window.
        store
            .record_provider(&submitted.job.id, provider_job, submitted.job.created_at_ms)
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
    assert_eq!(report.reconciled, 2);
    assert_eq!(report.deferred, 0);
    assert_eq!(provider.active_allocations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn each_launch_plan_rejection_names_the_field_at_fault() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let control = control_plane(&temp, provider).await.unwrap();
    let owner = principal("owner-a", 1_000_000);

    let mut overpriced = plan(1_800);
    overpriced.maximum_usdc_micros += 1;
    assert!(matches!(
        control
            .submit(&owner, "off-by-one", overpriced)
            .await
            .unwrap_err(),
        ServiceError::InvalidPlan(PlanRejection::Maximum {
            expected_usdc_micros: 360_000
        })
    ));

    let mut too_long = plan(1_800);
    too_long.duration_secs = 100_000;
    too_long.maximum_usdc_micros = 20_000_000;
    assert!(matches!(
        control
            .submit(&owner, "too-long", too_long)
            .await
            .unwrap_err(),
        ServiceError::InvalidPlan(PlanRejection::Duration {
            minimum_secs: MIN_DURATION_SECS,
            maximum_secs: 21_600
        })
    ));

    let mut preview = plan(1_800);
    preview.app = AppCatalog::builtin().app("comfyui").unwrap().clone();
    assert!(matches!(
        control
            .submit(&owner, "preview", preview)
            .await
            .unwrap_err(),
        ServiceError::InvalidPlan(PlanRejection::AppUnavailable)
    ));

    let mut unknown = plan(1_800);
    unknown.app.id = "not-in-the-catalog".into();
    assert!(matches!(
        control
            .submit(&owner, "unknown", unknown)
            .await
            .unwrap_err(),
        ServiceError::InvalidPlan(PlanRejection::UnknownApp)
    ));
}

#[tokio::test]
async fn a_price_ceiling_rejection_carries_the_expected_value_over_http() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let control = control_plane(&temp, provider).await.unwrap();
    let app = test_router(control);
    let mut overpriced = plan(1_800);
    overpriced.maximum_usdc_micros -= 1;

    let response = app
        .oneshot(
            Request::post("/v1/jobs")
                .header("content-type", "application/json")
                .header("authorization", format!("bearer {TEST_TOKEN}"))
                .header("idempotency-key", "off-by-one")
                .body(Body::from(serde_json::to_vec(&overpriced).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = axum::body::to_bytes(response.into_body(), 4_096)
        .await
        .unwrap();
    let envelope: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(envelope["error"]["code"], "invalid_maximum_usdc_micros");
    assert_eq!(
        envelope["error"]["message"],
        "maximum_usdc_micros must be 360000 for this offer and duration"
    );
}

#[tokio::test]
async fn the_catalog_is_served_over_http_behind_the_same_bearer_auth() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let control = control_plane(&temp, provider).await.unwrap();
    let app = test_router(control);

    let unauthorized = app
        .clone()
        .oneshot(Request::get("/v1/apps").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let response = app
        .oneshot(
            Request::get("/v1/apps")
                .header("authorization", format!("bearer {TEST_TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 16_384)
        .await
        .unwrap();
    let apps: Vec<covenant_compute::ComputeApp> = serde_json::from_slice(&body).unwrap();
    assert_eq!(apps, AppCatalog::builtin().apps());
}

#[tokio::test]
async fn unknown_routes_and_methods_answer_in_the_error_envelope() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let control = control_plane(&temp, provider).await.unwrap();
    let app = test_router(control);

    let unknown = app
        .clone()
        .oneshot(Request::get("/v1/nope").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    let body = axum::body::to_bytes(unknown.into_body(), 4_096)
        .await
        .unwrap();
    let envelope: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(envelope["error"]["code"], "unknown_route");

    let wrong_method = app
        .oneshot(Request::post("/v1/offers").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(wrong_method.status(), StatusCode::METHOD_NOT_ALLOWED);
    let body = axum::body::to_bytes(wrong_method.into_body(), 4_096)
        .await
        .unwrap();
    let envelope: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(envelope["error"]["code"], "method_not_allowed");
}

#[tokio::test]
async fn a_booking_shorter_than_provisioning_is_refused_with_the_accepted_range() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let control = control_plane(&temp, Arc::clone(&provider)).await.unwrap();
    let app = test_router(control);

    let response = app
        .oneshot(
            Request::post("/v1/jobs")
                .header("content-type", "application/json")
                .header("authorization", format!("bearer {TEST_TOKEN}"))
                .header("idempotency-key", "ten-seconds")
                .body(Body::from(serde_json::to_vec(&plan(10)).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = axum::body::to_bytes(response.into_body(), 4_096)
        .await
        .unwrap();
    let envelope: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(envelope["error"]["code"], "invalid_duration");
    assert_eq!(
        envelope["error"]["message"],
        "duration_secs must be between 300 and 21600"
    );
    assert_eq!(provider.allocations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_booking_at_the_minimum_duration_is_accepted() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let control = control_plane(&temp, Arc::clone(&provider)).await.unwrap();
    let owner = principal("owner-a", 1_000_000);

    let job = control
        .submit(&owner, "minimum-duration", plan(MIN_DURATION_SECS))
        .await
        .unwrap();

    assert_eq!(job.status, JobStatus::Running);
    assert_eq!(provider.allocations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_transitions_that_move_money_are_logged_with_the_job_id() {
    let logs = captured_logs();
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let control = control_plane(&temp, provider).await.unwrap();
    let owner = principal("owner-a", 1_000_000);

    let job = control
        .submit(&owner, "lifecycle-log", plan(1_800))
        .await
        .unwrap();
    control.cancel(&owner, &job.id).await.unwrap();

    let lines = logs.lines_for(&job.id);
    for expected in [
        "compute job is running and billing has started",
        "compute job cancellation requested",
        "compute provider teardown confirmed",
        "compute job settled",
    ] {
        assert!(
            lines.iter().any(|line| line.contains(expected)),
            "missing {expected} in {lines:#?}"
        );
    }
    assert!(lines
        .iter()
        .any(|line| line.contains("charged_usdc_micros")));
    assert!(!lines.iter().any(|line| line.contains("ephemeral-secret")));
}

#[tokio::test]
async fn a_reused_key_conflicts_even_after_the_offer_rotates() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(FakeProvider::new(vec![offer()]));
    let control = control_plane(&temp, provider).await.unwrap();
    let owner = principal("owner-a", 2_000_000);
    control
        .submit(&owner, "rotate-key", plan(1_800))
        .await
        .unwrap();

    // The offer the second request quotes has left the market, which used to
    // mask the conflict behind a stale-offer error.
    let mut rotated = plan(1_800);
    rotated.offer.id = "offer-gone".into();
    let error = control
        .submit(&owner, "rotate-key", rotated)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        ServiceError::Store(StoreError::IdempotencyConflict)
    ));
}
