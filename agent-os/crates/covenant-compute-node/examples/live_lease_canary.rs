//! Live proof: the whole renter loop against a real GPU market — rent a
//! real machine, hand its address to the buyer, hold it, close it, and
//! bill for the seconds it actually ran.
//!
//! **This spends real money.** It rents one GPU for well under a minute
//! and destroys it. Opt-in only: examples never run under `cargo test`
//! or `cargo build --workspace`.
//!
//! Needs:
//!   - `COVENANT_VAST_API_KEY` in the environment (a funded account)
//!   - network egress to the market's API
//!
//! For a REAL settlement (mainnet USDC to the operator, bound to the
//! lease receipt by an on-chain memo) also set:
//!   - `COVENANT_COMPUTE_PAYOUT_SIGNER` — path to a built
//!     `covenant-x402-signer` binary
//!   - `COVENANT_COMPUTE_FUNDING_KEYPAIR` — the custody keypair that
//!     funds payouts
//!   - `COVENANT_COMPUTE_RPC_URL` — a mainnet RPC
//!   - `COVENANT_COMPUTE_PAYOUT_ADDRESS` — the operator's wallet
//!
//! Without them the payout is recorded, not pushed, and the run still
//! proves the rental and the meter.
//!
//! Run from `agent-os/`:
//!   COVENANT_VAST_API_KEY=... cargo run -p covenant-compute-node \
//!     --example live_lease_canary
//!
//! The buyer, operator and coordinator identities are all freshly
//! generated here; the escrow is the coordinator's own custodial ledger
//! (no chain), so the only real value at risk is the market rental
//! itself. Every exit path destroys the instance — including the panic
//! path, which is why the teardown is a guard rather than a final
//! statement.

use std::sync::Arc;
use std::time::{Duration, Instant};

use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
use covenant_audit::{AuditLog, InMemoryAuditLog};
use covenant_compute_coordinator::payout::{Payout, SidecarPayout, SidecarPayoutConfig};
use covenant_compute_coordinator::{
    payout::MockPayout, reputation::NoReputation, CoordinatorConfig, CoordinatorState,
};
use covenant_compute_node::coordinator::Coordinator;
use covenant_compute_node::lease::SessionBackend;
use covenant_compute_node::{
    BrokerConfig, BrokerSessionBackend, HttpCoordinatorClient, InMemoryEarningsLedger,
    LeaseControl, LeaseExecutor, Node, NodeConfig,
};
use covenant_compute_protocol::{
    lease_input, CapabilityProfile, CapabilityRequirement, HardwareClass, JobEnvelopePayload,
    JobKind, LeaseCloseRequest, LeaseTerms, PriceAsk, PriceUnit, RegisterRequest,
    SignedJobEnvelope,
};
use covenant_compute_vast::{ApiToken, VastClient, VastConfig};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use uuid::Uuid;

/// The image every session runs, pinned by digest.
const IMAGE: &str = "docker.io/nvidia/cuda@sha256:\
                     cff3a0d82d2c2b47bab252d67fa9b34a20ef4c50781d98501b5c7367ea9afd10";
/// Ceiling on what the broker will pay per hour: $1.20.
const MAX_HOURLY_MICROS: u64 = 1_200_000;
/// What the renter is charged per second of session.
const RATE_MICRO_USDC_PER_SEC: u64 = 100;
/// The window the renter escrows: 10 minutes at the rate above.
const WINDOW_SECS: u64 = 600;
/// How long the session is held before the renter closes it.
const HOLD: Duration = Duration::from_secs(20);

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[tokio::main]
async fn main() {
    let token = ApiToken::from_environment()
        .expect("COVENANT_VAST_API_KEY must be set to a funded market account");
    let vast = Arc::new(
        VastClient::new(
            VastConfig {
                max_hourly_micros: MAX_HOURLY_MICROS,
                ..VastConfig::from_environment().expect("market config")
            },
            token,
        )
        .expect("market client"),
    );

    // A real coordinator, in this process. The escrow is its own
    // custodial ledger and payouts are recorded rather than pushed —
    // this canary proves the rental and the meter, not the chain leg,
    // which is proven separately on devnet.
    let coordinator_identity = LocalIdentity::generate("coordinator@canary");
    // Real settlement when the operator supplied a signer and a funding
    // key; a recorded intent otherwise. The lease loop is identical
    // either way — settlement is a backend, not a code path.
    let signer = std::env::var("COVENANT_COMPUTE_PAYOUT_SIGNER").ok();
    let funding = std::env::var("COVENANT_COMPUTE_FUNDING_KEYPAIR").ok();
    let mock_payout = Arc::new(MockPayout::new());
    let (payout, settling_onchain): (Arc<dyn Payout>, bool) = match (&signer, &funding) {
        (Some(signer), Some(funding)) => {
            println!("settlement: REAL — mainnet USDC through {signer}");
            (
                Arc::new(SidecarPayout::new(SidecarPayoutConfig {
                    signer_binary: signer.into(),
                    rpc_url: std::env::var("COVENANT_COMPUTE_RPC_URL")
                        .expect("COVENANT_COMPUTE_RPC_URL is required for a real settlement"),
                    funding_keypair_path: funding.clone(),
                    mint: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".into(),
                    // A lease's whole window is the most one payout can
                    // ever move; the cap is that, not a round number.
                    cap_micro_usdc: RATE_MICRO_USDC_PER_SEC * WINDOW_SECS,
                    obligation_cap_micro_usdc: 0,
                })),
                true,
            )
        }
        _ => {
            println!("settlement: recorded only (no signer configured)");
            (mock_payout.clone(), false)
        }
    };
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let state = CoordinatorState::new(
        coordinator_identity,
        CoordinatorConfig {
            long_poll_timeout: Duration::from_secs(10),
            ..CoordinatorConfig::default()
        },
        Arc::new(NoReputation),
        payout,
        audit.clone(),
    );
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    {
        let state = state.clone();
        tokio::spawn(async move {
            let app = covenant_compute_coordinator::http::router(state);
            axum::serve(listener, app).await.unwrap();
        });
    }
    println!("coordinator listening on {base_url}");

    // The broker operator: owns no hardware, rents per session.
    let operator_identity = LocalIdentity::generate("operator@canary");
    let profile = CapabilityProfile {
        operator: operator_identity.agent_id(),
        hardware: HardwareClass::CpuOnly,
        vram_gb: 0,
        models_served: vec!["any".into()],
        job_kinds: vec![JobKind::LeaseSession],
        price: PriceAsk {
            // A lease prices by the GPU-hour: the floor scales that rate to
            // the metered window, so an early close still pays per second.
            unit: PriceUnit::PerLeaseHour,
            micro_usdc: RATE_MICRO_USDC_PER_SEC * 3_600,
        },
        tee_capable: false,
        kind_prices: Vec::new(),
        kind_models: Vec::new(),
    };
    let client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(20),
        2,
    ));
    let registered = client
        .register(
            RegisterRequest::sign(
                profile.clone(),
                std::env::var("COVENANT_COMPUTE_PAYOUT_ADDRESS")
                    .unwrap_or_else(|_| "9VaDVp1Wb78G4Wm6VuTiMrpESjrUymXefQTHcJGRSTEA".into()),
                &operator_identity,
            )
            .unwrap(),
        )
        .await
        .expect("register");
    assert!(registered.accepted);

    let control = LeaseControl::new();
    let broker = Arc::new(BrokerSessionBackend::new(
        vast.clone(),
        BrokerConfig {
            image: IMAGE.into(),
            max_hourly_micros: MAX_HOURLY_MICROS,
            ready_timeout: Duration::from_secs(420),
            ready_poll_interval: Duration::from_secs(5),
        },
    ));
    let node = Node::new(
        operator_identity,
        profile,
        client.clone(),
        Arc::new(
            LeaseExecutor::new(broker.clone(), control.clone())
                .watching(client.clone())
                .with_poll_interval(Duration::from_millis(500)),
        ),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 1,
            preempt_grace: Duration::from_secs(5),
            fee_bps: 0,
        },
    );

    // The renter. Their own key is what makes the session theirs.
    let buyer = LocalIdentity::generate("buyer@canary");
    let ssh_public_key = std::env::var("COVENANT_CANARY_SSH_PUBLIC_KEY").unwrap_or_else(|_| {
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIB6Y7dJdPGCcTGxHKGdRJmQnPRxCbcHmtFTMYPFhLbHm \
         canary@covenant"
            .to_string()
    });
    let terms = LeaseTerms {
        max_duration_secs: WINDOW_SECS,
        rate_micro_usdc_per_sec: RATE_MICRO_USDC_PER_SEC,
        client_public_key: Some(ssh_public_key),
    };
    let ceiling = terms.max_price_micro_usdc().unwrap();
    let job_id = Uuid::new_v4();
    let payload = JobEnvelopePayload {
        job_id,
        buyer: buyer.agent_id(),
        kind: JobKind::LeaseSession,
        capability_requirement: CapabilityRequirement {
            gpu_class: None,
            min_vram_gb: None,
            model_id: None,
            kind: JobKind::LeaseSession,
            max_duration_secs: WINDOW_SECS as u32,
            min_reputation_bps: None,
        },
        input: vec![
            lease_input(terms.clone()).unwrap(),
            Content::text("covenant compute live lease canary"),
        ],
        price_micro_usdc: ceiling,
        deadline_ms: WINDOW_SECS * 1_000 + covenant_compute_protocol::LEASE_DEADLINE_SLACK_MS,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "live-lease-canary"),
        issued_at_ms: epoch_ms(),
        referral_code: None,
        stream: true,
    };
    let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();

    let http = reqwest::Client::new();
    let submitted = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .expect("submit");
    println!(
        "lease submitted: {job_id} · escrowed {ceiling} micro-USDC \
         ({RATE_MICRO_USDC_PER_SEC}/s x {WINDOW_SECS}s) · {}",
        submitted.status()
    );

    // Serve it. From here on a real machine may exist, so every exit
    // must destroy it — including a panic.
    let serving = tokio::spawn(async move {
        let outcome = node.run_once().await;
        match &outcome {
            Ok(Some(o)) => println!(
                "[node] served {} status={:?} error={:?}",
                o.job_id, o.receipt.receipt.status, o.error_message
            ),
            Ok(None) => println!("[node] no job was offered"),
            Err(e) => println!("[node] failed: {e}"),
        }
        outcome
    });
    let guard = TeardownGuard {
        broker: broker.clone(),
        job_id,
    };

    let lease_path = format!("/federation/jobs/{job_id}/lease");
    let lease_url = format!("{base_url}{lease_path}");
    let renting_started = Instant::now();
    let mut endpoint = None;
    while renting_started.elapsed() < Duration::from_secs(450) {
        // The access grant returns only to the lease's buyer, so sign the
        // read the way the buyer client does.
        let signed_at_ms = epoch_ms();
        let signature =
            covenant_compute_protocol::sign_read(&buyer, &lease_path, signed_at_ms).unwrap();
        let view: serde_json::Value = http
            .get(&lease_url)
            .header(
                covenant_compute_protocol::READ_SIGNED_AT_HEADER,
                signed_at_ms.to_string(),
            )
            .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, signature)
            .send()
            .await
            .expect("lease view")
            .json()
            .await
            .expect("lease json");
        if let Some(found) = view.pointer("/access/endpoint").and_then(|v| v.as_str()) {
            println!(
                "\n=== SESSION LIVE after {:?} ===\n  {found}\n  {}",
                renting_started.elapsed(),
                view.pointer("/access/note")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
            );
            endpoint = Some(found.to_string());
            break;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    let Some(endpoint) = endpoint else {
        // The node's own outcome is the only place the market's refusal
        // is spelled out; a bare panic here would hide it.
        let _ = serving.await;
        panic!("no reachable machine — see the [node] line above");
    };

    // Watch the meter move while the renter "uses" the box.
    tokio::time::sleep(HOLD).await;
    let running: serde_json::Value = http
        .get(&lease_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    println!(
        "meter at {}ms: {} micro-USDC of a {ceiling} ceiling",
        running["elapsed_ms"], running["charged_micro_usdc"]
    );

    // The renter is done.
    let close = LeaseCloseRequest::sign(buyer.agent_id(), job_id, epoch_ms(), &buyer).unwrap();
    let closed: serde_json::Value = http
        .post(format!("{base_url}/federation/jobs/{job_id}/close"))
        .json(&close)
        .send()
        .await
        .expect("close")
        .json()
        .await
        .unwrap();
    println!("close requested: {}", closed["close_requested"]);

    let outcome = serving
        .await
        .expect("serve task")
        .expect("run_once")
        .expect("the lease was served");
    println!("receipt status: {:?}", outcome.receipt.receipt.status);

    let record = state.jobs().get(job_id).expect("job record");
    let billed_ms = record.metered_elapsed_ms.expect("a metered lease");
    let charged = terms.metered_micro_usdc(billed_ms);
    let (settled, _) = state.escrow().hold_info(job_id).expect("hold");
    let buyer_charged = state
        .escrow()
        .organic_charged(&buyer.agent_id().pubkey_base58());

    println!("\n=== RECEIPT ===");
    println!("  job              {job_id}");
    println!("  endpoint         {endpoint}");
    println!("  session ran      {billed_ms} ms");
    println!("  rate             {RATE_MICRO_USDC_PER_SEC} micro-USDC/s");
    println!("  escrowed         {ceiling} micro-USDC (the {WINDOW_SECS}s window)");
    println!("  charged          {charged} micro-USDC");
    println!("  returned         {} micro-USDC", ceiling - charged);
    println!("  settled hold     {settled} micro-USDC");
    println!("  buyer's books    {buyer_charged} micro-USDC charged");
    let paid = state
        .jobs()
        .get(job_id)
        .and_then(|r| r.payout)
        .expect("a settled lease pins its payout");
    println!("  operator paid    {} micro-USDC", paid.amount_micro_usdc);
    match &paid.tx_signature {
        Some(sig) => println!("  settlement tx    {sig}"),
        None => println!("  settlement tx    (recorded only, nothing submitted)"),
    }
    if settling_onchain {
        assert!(
            paid.tx_signature.is_some(),
            "a real settlement must carry the transaction that moved the money"
        );
    }
    let _ = &mock_payout;

    assert_eq!(settled, charged, "the hold settles at the metered charge");
    assert_eq!(buyer_charged, charged, "the unused window went back");
    assert!(
        charged < ceiling,
        "an early close must cost less than the whole window"
    );

    drop(guard);
    // Belt and braces: prove no instance of ours is still alive.
    match vast.recover(&job_id.to_string()).await {
        Ok(alive) if alive.is_empty() => println!("\nno instances left running — clean"),
        Ok(alive) => println!("\nSTILL RUNNING (destroy by hand): {alive:?}"),
        Err(e) => println!("\ncould not confirm teardown: {e}"),
    }
}

/// Destroys the rented machine on every exit path, panic included.
struct TeardownGuard {
    broker: Arc<BrokerSessionBackend>,
    job_id: Uuid,
}

impl Drop for TeardownGuard {
    fn drop(&mut self) {
        let broker = self.broker.clone();
        let job_id = self.job_id;
        // Drop can't await; hand the teardown to a blocking thread with
        // its own runtime so a panic still returns the machine.
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("teardown runtime");
            rt.block_on(broker.close(job_id));
        })
        .join();
    }
}
