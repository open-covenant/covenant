//! The engine-backed control plane end to end: a real coordinator with a
//! journaled money core, a registered lease node serving a stub session,
//! and the control plane in front of it as a funded buyer client. The unit
//! tests project a lease onto the customer job shape against an in-memory
//! provider; this drives the customer HTTP surface — browse offers, launch,
//! read, close — through the live [`EngineProvider`], so the async glue the
//! projection tests cannot reach is exercised against a coordinator that
//! actually holds escrow and settles a meter.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::Router;
use covenant_compute_buyer::{claim_deposit, BuyerConfig};
use covenant_compute_control::{
    quote_maximum, router, AppCatalog, AuthRegistry, BetaCredential, ComputeJob, ComputeOffer,
    ControlPlane, EngineProvider, JobStatus, LaunchPlan, LaunchRequest,
};
use covenant_compute_coordinator::{
    router as compute_router, CoordinatorConfig, CoordinatorState, MockPayout, MockRail,
    NoReputation, VerifiedDeposit,
};
use covenant_compute_node::{
    Coordinator as _, HttpCoordinatorClient, InMemoryEarningsLedger, LeaseControl, LeaseExecutor,
    Node, NodeConfig, StubSessionBackend,
};
use covenant_compute_protocol::{
    CapabilityProfile, FundingSource, HardwareClass, JobKind, PriceAsk, PriceUnit, RegisterRequest,
};
use covenant_identity::LocalIdentity;
use serde::de::DeserializeOwned;
use tower::ServiceExt;

const TOKEN: &str = "control-plane-e2e-secret-token";
/// The stub lease node's published address, handed back verbatim to the
/// customer once the session is reachable.
const STUB_ENDPOINT: &str = "ssh renter@stub.test -p 2222";
/// The operator prices the GPU-hour at a whole micro-USDC per second
/// (3_600_000 / 3600 = 1000), so the per-hour quote a customer sees equals
/// the per-second escrow the lease opens with, and the matcher's window
/// floor lands on the same figure.
const HOURLY_ASK_MICRO_USDC: u64 = 3_600_000;

/// One live market fronted by the control plane: a journaled coordinator, a
/// lease node serving a stub session over its real wire, and a funded
/// control-plane buyer whose HTTP surface the test drives.
struct Harness {
    app: Router,
    url: String,
    _coordinator_home: tempfile::TempDir,
    _buyer_home: tempfile::TempDir,
}

impl Harness {
    async fn launch() -> Self {
        Self::launch_with(true).await
    }

    /// A market whose lease node registers its offer but never accepts, so a
    /// launched lease stays offered — the state a buyer cancels rather than
    /// closes.
    async fn launch_idle() -> Self {
        Self::launch_with(false).await
    }

    async fn launch_with(accepting: bool) -> Self {
        let coordinator_home = tempfile::tempdir().unwrap();
        let rail = Arc::new(MockRail::new());
        let state = CoordinatorState::with_journal(
            LocalIdentity::generate("coordinator@test"),
            CoordinatorConfig {
                long_poll_timeout: Duration::from_secs(5),
                default_funding_source: FundingSource::Organic,
                ..CoordinatorConfig::default()
            },
            Arc::new(NoReputation),
            Arc::new(MockPayout::new()),
            Arc::new(covenant_audit::InMemoryAuditLog::new()),
            &coordinator_home.path().join("journal.jsonl"),
            Some(rail.clone()),
        )
        .await
        .unwrap();
        let coordinator_pubkey = state.coordinator_pubkey_b58();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, compute_router(state)).await.unwrap();
        });

        register_lease_node(&url, coordinator_pubkey, accepting).await;

        let buyer_home = tempfile::tempdir().unwrap();
        let buyer = LocalIdentity::load_or_create(
            &buyer_home.path().join("identity.json"),
            "control@compute",
        )
        .unwrap();
        rail.preload(VerifiedDeposit {
            deposit_id: "control-deposit".into(),
            buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
            amount_micro_usdc: 1_000_000,
        });
        let config = BuyerConfig {
            coordinator_url: url.clone(),
            poll_interval: Duration::from_millis(100),
            referral_code: None,
            rpc_url: None,
        };
        claim_deposit(&reqwest::Client::new(), &config, &buyer, "control-deposit")
            .await
            .unwrap();

        let provider = EngineProvider::new(config, buyer, 1_000_000, None);
        let auth = Arc::new(
            AuthRegistry::new(vec![BetaCredential {
                owner: "beta".into(),
                token: TOKEN.into(),
                spend_cap_usdc_micros: 1_000_000,
            }])
            .unwrap(),
        );
        let control = ControlPlane::new(AppCatalog::builtin(), Arc::new(provider));

        Self {
            app: router(auth, control),
            url,
            _coordinator_home: coordinator_home,
            _buyer_home: buyer_home,
        }
    }

    async fn send(&self, request: Request<Body>) -> Response {
        self.app.clone().oneshot(request).await.unwrap()
    }

    /// The coordinator's own account of a lease, read straight off its
    /// engine — the ground truth behind the customer job projection, used
    /// only to diagnose a stuck lease.
    async fn raw_lease(&self, job_id: &str) -> String {
        let config = BuyerConfig {
            coordinator_url: self.url.clone(),
            poll_interval: Duration::from_millis(100),
            referral_code: None,
            rpc_url: None,
        };
        let id = job_id.parse().unwrap();
        let buyer = LocalIdentity::load_or_create(
            &self._buyer_home.path().join("identity.json"),
            "control@compute",
        )
        .unwrap();
        match covenant_compute_buyer::lease_view(&reqwest::Client::new(), &config, &buyer, id).await
        {
            Ok(view) => format!("{view:?}"),
            Err(e) => format!("lease_view error: {e}"),
        }
    }
}

/// Registers a datacenter-GPU node that serves lease sessions from a stub
/// backend. The backend publishes a fixed endpoint and holds the session
/// open until the buyer's close, so the whole lease lifecycle drives
/// without renting a real machine — the same wire a real lease node speaks.
/// With `accepting`, the node runs its accept/serve loop for the test's
/// lifetime; without it the offer is on the market but no lease is ever
/// accepted, so a launch stays offered.
async fn register_lease_node(url: &str, coordinator_pubkey: String, accepting: bool) {
    let operator = LocalIdentity::generate("operator@test");
    let profile = CapabilityProfile {
        operator: operator.agent_id(),
        hardware: HardwareClass::ConsumerGpu {
            model: "rtx-4090".into(),
        },
        vram_gb: 24,
        models_served: vec!["any".into()],
        job_kinds: vec![JobKind::LeaseSession],
        price: PriceAsk {
            unit: PriceUnit::PerLeaseHour,
            micro_usdc: HOURLY_ASK_MICRO_USDC,
        },
        tee_capable: false,
    };
    let client = Arc::new(HttpCoordinatorClient::with_config(
        url.to_owned(),
        Duration::from_secs(5),
        2,
    ));
    client
        .register(RegisterRequest::sign(profile.clone(), payout_addr(7), &operator).unwrap())
        .await
        .unwrap();
    if !accepting {
        return;
    }
    let executor = LeaseExecutor::new(
        Arc::new(StubSessionBackend::new(STUB_ENDPOINT)),
        LeaseControl::new(),
    )
    .watching(client.clone())
    .with_poll_interval(Duration::from_millis(100));
    let node = Node::new(
        operator,
        profile,
        client,
        Arc::new(executor),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(covenant_audit::InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58: coordinator_pubkey,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(1),
            fee_bps: 0,
        },
    );
    tokio::spawn(async move {
        loop {
            match node.run_once().await {
                Ok(Some(_)) => {}
                _ => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
    });
}

fn payout_addr(seed: u8) -> String {
    bs58::encode([seed; 32]).into_string()
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap()
}

fn delete(uri: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap()
}

fn post_launch(plan: &LaunchPlan, idempotency_key: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/jobs")
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .header("idempotency-key", idempotency_key)
        .body(Body::from(serde_json::to_vec(plan).unwrap()))
        .unwrap()
}

fn post_plan(request: &LaunchRequest) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/plans")
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(request).unwrap()))
        .unwrap()
}

async fn json_body<T: DeserializeOwned>(response: Response) -> T {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        panic!(
            "decode failed: {e}; body={}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

/// A launch quoting the live offer for the released GPU workspace at the
/// shortest allowed window.
fn workspace_plan(offer: ComputeOffer) -> LaunchPlan {
    let app = AppCatalog::builtin().app("gpu-workspace").unwrap().clone();
    let duration_secs = 300;
    let maximum_usdc_micros =
        quote_maximum(offer.rate_usdc_micros_per_hour, duration_secs).unwrap();
    LaunchPlan {
        app,
        offer,
        duration_secs,
        maximum_usdc_micros,
    }
}

/// Reads the single offer the registered lease node contributes to the
/// live market.
async fn live_offer(harness: &Harness) -> ComputeOffer {
    let response = harness.send(get("/v1/offers")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let offers: Vec<ComputeOffer> = json_body(response).await;
    assert_eq!(offers.len(), 1, "one lease node contributes one offer");
    assert_eq!(offers[0].id, "rtx-4090");
    assert_eq!(offers[0].rate_usdc_micros_per_hour, HOURLY_ASK_MICRO_USDC);
    assert!(offers[0].online);
    offers[0].clone()
}

/// Polls one job until it reaches `want`, refreshing it against the
/// coordinator each read the way a workspace client does.
async fn wait_for(harness: &Harness, job_id: &str, want: JobStatus) -> ComputeJob {
    for _ in 0..100 {
        let response = harness.send(get(&format!("/v1/jobs/{job_id}"))).await;
        assert_eq!(response.status(), StatusCode::OK);
        let job: ComputeJob = json_body(response).await;
        if job.status == want {
            return job;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!(
        "job {job_id} never reached {want:?}; coordinator lease: {}",
        harness.raw_lease(job_id).await
    );
}

#[tokio::test]
async fn a_launch_runs_and_closes_through_the_live_engine() {
    let harness = Harness::launch().await;

    let offer = live_offer(&harness).await;
    let plan = workspace_plan(offer);

    let response = harness.send(post_launch(&plan, "launch-1")).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "launch should open a lease against the live engine"
    );
    let launched: ComputeJob = json_body(response).await;
    let job_id = launched.id.clone();
    assert_eq!(launched.app_id, "gpu-workspace");
    assert_eq!(launched.offer_id, "rtx-4090");
    // rate 1000 micro/s over the 300s window escrows exactly the quote.
    assert_eq!(launched.maximum_usdc_micros, 300_000);

    // The node accepts the offered lease and publishes its endpoint; the
    // control plane surfaces it as a running job carrying the address.
    let running = wait_for(&harness, &job_id, JobStatus::Running).await;
    assert_eq!(running.access_url.as_deref(), Some(STUB_ENDPOINT));
    assert!(running.receipt.is_none());

    // Closing requests the close on the coordinator; the node sees it on
    // its next poll, releases the session and submits its receipt. The
    // close is recorded, not settled on the spot, so the immediate answer
    // is the session winding down: stopping, with the credential it was
    // asked to release already withdrawn.
    let response = harness.send(delete(&format!("/v1/jobs/{job_id}"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let stopping: ComputeJob = json_body(response).await;
    assert_eq!(stopping.status, JobStatus::Stopping);
    assert!(
        stopping.access_url.is_none(),
        "a stopping session no longer advertises its credential"
    );
    assert!(stopping.receipt.is_none());

    // It settles to completed once the node's receipt lands.
    let settled = wait_for(&harness, &job_id, JobStatus::Completed).await;
    assert!(
        settled.access_url.is_none(),
        "a closed session drops its credential"
    );
    let receipt = settled.receipt.expect("a settled lease carries a receipt");
    assert_eq!(receipt.provider, "covenant");
    assert!(
        receipt.charged_usdc_micros < 300_000,
        "an early close bills the seconds served, not the whole window: {receipt:?}"
    );
    assert_eq!(
        receipt.charged_usdc_micros + receipt.refunded_usdc_micros,
        300_000,
        "charge and refund reconcile to the escrowed ceiling"
    );

    // Re-reading a terminal job is served from the record; the coordinator
    // is no longer consulted.
    let reread = harness.send(get(&format!("/v1/jobs/{job_id}"))).await;
    assert_eq!(reread.status(), StatusCode::OK);
    assert_eq!(
        json_body::<ComputeJob>(reread).await.status,
        JobStatus::Completed
    );
}

#[tokio::test]
async fn a_resolved_plan_launches_through_the_live_engine() {
    let harness = Harness::launch().await;

    // Resolve against the live market instead of hand-building the plan:
    // name the app, the window and a budget, and let the control plane pick
    // and price the one offer the lease node contributes.
    let request = LaunchRequest {
        app_id: "gpu-workspace".into(),
        duration_secs: 300,
        max_usdc_micros: 500_000,
        min_trust: None,
    };
    let response = harness.send(post_plan(&request)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let plan: LaunchPlan = json_body(response).await;
    assert_eq!(plan.offer.id, "rtx-4090");
    // rate 1000 micro/s over the 300s window prices at exactly the quote.
    assert_eq!(plan.maximum_usdc_micros, 300_000);

    // The resolved plan commits and runs like any other launch.
    let launched: ComputeJob =
        json_body(harness.send(post_launch(&plan, "resolved-1")).await).await;
    let job_id = launched.id.clone();
    assert_eq!(launched.maximum_usdc_micros, 300_000);

    let running = wait_for(&harness, &job_id, JobStatus::Running).await;
    assert_eq!(running.access_url.as_deref(), Some(STUB_ENDPOINT));

    harness.send(delete(&format!("/v1/jobs/{job_id}"))).await;
    let settled = wait_for(&harness, &job_id, JobStatus::Completed).await;
    let receipt = settled.receipt.expect("a settled lease carries a receipt");
    assert_eq!(
        receipt.charged_usdc_micros + receipt.refunded_usdc_micros,
        300_000,
        "charge and refund reconcile to the escrowed ceiling"
    );
}

#[tokio::test]
async fn a_repeated_launch_key_replays_the_same_lease() {
    let harness = Harness::launch().await;
    let plan = workspace_plan(live_offer(&harness).await);

    let first: ComputeJob = json_body(harness.send(post_launch(&plan, "dup")).await).await;
    let replay = harness.send(post_launch(&plan, "dup")).await;
    assert_eq!(replay.status(), StatusCode::OK);
    let replay: ComputeJob = json_body(replay).await;
    assert_eq!(
        replay.id, first.id,
        "a repeated idempotency key returns the same lease, not a second machine"
    );

    // The listing holds exactly the one lease the owner opened.
    let listing = harness.send(get("/v1/jobs")).await;
    assert_eq!(listing.status(), StatusCode::OK);
    let jobs: Vec<ComputeJob> = json_body(listing).await;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].id, first.id);
}

#[tokio::test]
async fn a_provisioning_job_cancels_and_refunds() {
    // The lease node registers its offer but never accepts, so the launch
    // sits provisioning. A buyer that changes its mind cancels it and gets
    // the whole escrowed ceiling back at once, rather than waiting out the
    // lease deadline for the refund.
    let harness = Harness::launch_idle().await;
    let plan = workspace_plan(live_offer(&harness).await);

    let launched: ComputeJob = json_body(harness.send(post_launch(&plan, "cancel-1")).await).await;
    assert_eq!(launched.status, JobStatus::Provisioning);
    assert!(launched.access_url.is_none());
    let job_id = launched.id.clone();

    let response = harness.send(delete(&format!("/v1/jobs/{job_id}"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let cancelled: ComputeJob = json_body(response).await;
    assert_eq!(cancelled.status, JobStatus::Cancelled);
    assert!(cancelled.access_url.is_none());
    let receipt = cancelled
        .receipt
        .expect("a cancelled lease carries a receipt");
    assert_eq!(
        receipt.charged_usdc_micros, 0,
        "an unaccepted lease never ran"
    );
    assert_eq!(receipt.refunded_usdc_micros, plan.maximum_usdc_micros);

    // The cancelled job settles terminal in the record, and cancelling is
    // safe to repeat.
    let again = harness.send(delete(&format!("/v1/jobs/{job_id}"))).await;
    assert_eq!(again.status(), StatusCode::OK);
    assert_eq!(
        json_body::<ComputeJob>(again).await.status,
        JobStatus::Cancelled
    );
}
