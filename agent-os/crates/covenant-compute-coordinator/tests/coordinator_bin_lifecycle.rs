//! The `covenant-compute-coordinator` binary end to end: a real
//! spawned process configured through its environment — boot, a paid
//! job at a disclosed fee, SIGTERM, and a second life over the same
//! home that answers for the first life's books with the same
//! identity. The hermetic twin of the deployed service.

#![cfg(unix)]

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use covenant_compute_buyer::{
    dispatch_and_verify, list_verified_jobs, sign_envelope, BuyerConfig, JobRequest,
};
use covenant_compute_node::{
    Coordinator as _, EchoExecutor, HttpCoordinatorClient, InMemoryEarningsLedger, Node, NodeConfig,
};
use covenant_compute_protocol::{
    CapabilityProfile, HardwareClass, JobKind, PriceAsk, PriceUnit, RegisterRequest,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use tokio::process::{Child, Command};
use uuid::Uuid;

async fn free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

/// A payable operator payout address per fixture seed — registration
/// refuses anything that doesn't decode to a 32-byte key.
fn payout_addr(seed: u8) -> String {
    bs58::encode([seed; 32]).into_string()
}

/// The deployed invocation: no arguments, everything through the
/// environment. Fee and a short long-poll are set so the test can
/// observe the disclosure and drain fast.
fn spawn_coordinator(home: &std::path::Path, port: u16) -> Child {
    Command::new(env!("CARGO_BIN_EXE_covenant-compute-coordinator"))
        .env_clear()
        .env("COVENANT_COMPUTE_COORDINATOR_HOME", home)
        .env("COVENANT_COMPUTE_COORDINATOR_PORT", port.to_string())
        .env("COVENANT_COMPUTE_LONG_POLL_SECS", "1")
        .env("COVENANT_COMPUTE_FEE_BPS", "250")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenant-compute-coordinator")
}

/// A spawned coordinator that exited before it ever answered `/health`,
/// carrying its exit status. Nearly always the `free_port`
/// bind-drop-rebind handoff losing the port to a concurrent suite; a
/// fresh port clears it, so [`spawn_healthy`] retries on this.
struct DiedBeforeHealthy(String);

/// Polls `/health` until the spawned binary answers, watching the child
/// the whole time. A parallel full-suite run compiles and boots several
/// binaries at once, so a healthy boot can take tens of seconds under
/// load: that deserves patience, not a flake. A child that dies before
/// it serves returns `Err` for the caller to retry on a fresh port; a
/// child that boots but never turns healthy is a real hang and fails.
async fn wait_healthy(coordinator: &mut Child, url: &str) -> Result<(), DiedBeforeHealthy> {
    let http = reqwest::Client::new();
    for _ in 0..1_200 {
        if let Some(status) = coordinator
            .try_wait()
            .expect("poll the spawned coordinator")
        {
            return Err(DiedBeforeHealthy(status.to_string()));
        }
        if let Ok(resp) = http.get(format!("{url}/health")).send().await {
            if resp.status().is_success() {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the coordinator booted but never answered /health within 120s");
}

/// Spawn a coordinator on a free port and wait until it is healthy,
/// retrying past the one flake this harness has: `free_port` binds a
/// port, drops it, and hands the number to the child, so a concurrent
/// suite can grab the port in the gap and the child dies at bind. A
/// child that dies before it serves is respawned on a fresh port; a
/// booted-but-unhealthy child fails inside [`wait_healthy`]. Returns the
/// live child and the URL it bound. `build` takes the chosen port and
/// returns the spawned process.
async fn spawn_healthy(mut build: impl FnMut(u16) -> Child) -> (Child, String) {
    let mut last_exit = String::new();
    for _ in 0..8 {
        let port = free_port().await;
        let url = format!("http://127.0.0.1:{port}");
        let mut coordinator = build(port);
        match wait_healthy(&mut coordinator, &url).await {
            Ok(()) => return (coordinator, url),
            Err(DiedBeforeHealthy(status)) => last_exit = status,
        }
    }
    panic!("the coordinator lost its port on 8 successive attempts (last exit: {last_exit})");
}

async fn operators_registered(url: &str) -> u64 {
    let body = reqwest::get(format!("{url}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    body.lines()
        .find_map(|l| l.strip_prefix("compute_operators_registered "))
        .expect("the registry gauge is scrapable")
        .trim()
        .parse()
        .unwrap()
}

/// Register a fresh echo operator and serve jobs until aborted.
/// Registration doubles as the fee-disclosure probe: the bin's
/// `COVENANT_COMPUTE_FEE_BPS` must come back on the wire.
async fn serve_echo_node(url: &str, coordinator_pubkey: &str) -> tokio::task::JoinHandle<()> {
    let operator = LocalIdentity::generate("operator@test");
    let profile = CapabilityProfile {
        operator: operator.agent_id(),
        hardware: HardwareClass::CpuOnly,
        vram_gb: 0,
        models_served: vec!["any".into()],
        job_kinds: vec![JobKind::InferenceCall],
        price: PriceAsk {
            unit: PriceUnit::PerJob,
            micro_usdc: 10_000,
        },
        tee_capable: false,
        kind_prices: Vec::new(),
        kind_models: Vec::new(),
    };
    let client = Arc::new(HttpCoordinatorClient::with_config(
        url.to_string(),
        Duration::from_secs(5),
        2,
    ));
    let response = client
        .register(RegisterRequest::sign(profile.clone(), payout_addr(2), &operator).unwrap())
        .await
        .unwrap();
    assert!(response.accepted);
    assert_eq!(
        response.fee_bps, 250,
        "the env-configured fee is disclosed at registration"
    );
    let node = Node::new(
        operator,
        profile,
        client,
        Arc::new(EchoExecutor),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(covenant_audit::InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58: coordinator_pubkey.to_string(),
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(1),
            fee_bps: response.fee_bps,
        },
    );
    tokio::spawn(async move {
        loop {
            match node.run_once().await {
                Ok(Some(_)) => {}
                _ => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
    })
}

async fn sigterm_and_wait(child: &mut Child) {
    let pid = child.id().expect("running child has a pid");
    let killed = std::process::Command::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let status = tokio::time::timeout(Duration::from_secs(15), child.wait())
        .await
        .expect("graceful exit within 15s of SIGTERM")
        .expect("wait on coordinator");
    assert!(
        status.success(),
        "a graceful shutdown exits 0, got {status}"
    );
}

#[tokio::test]
async fn the_coordinator_binary_keeps_its_identity_and_books_across_a_restart() {
    let home = tempfile::tempdir().unwrap();

    // Life 1: boot from a clean home, serve one paid job.
    let (mut coordinator, url) = spawn_healthy(|port| spawn_coordinator(home.path(), port)).await;

    // The identity the boot minted is the pubkey operators pin
    // out-of-band; read it the way an operator would be handed it.
    let pinned =
        LocalIdentity::load_or_create(&home.path().join("identity.json"), "coordinator@compute")
            .unwrap()
            .agent_id()
            .pubkey_base58();

    let node_loop = serve_echo_node(&url, &pinned).await;

    let http = reqwest::Client::new();
    let config = BuyerConfig {
        coordinator_url: url.clone(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    let buyer = LocalIdentity::generate("buyer@test");
    let outcome = dispatch_and_verify(
        &http,
        &config,
        &buyer,
        JobRequest {
            kind: JobKind::InferenceCall,
            input: vec![Content::text("the coordinator binary question")],
            model: None,
            gpu_class: None,
            min_vram_gb: None,
            min_reputation_bps: None,
            price_micro_usdc: 25_000,
            deadline_ms: 30_000,
        },
    )
    .await
    .expect("the spawned coordinator settles the job");
    match &outcome.output[0] {
        Content::Text { text } => assert_eq!(text, "the coordinator binary question"),
        other => panic!("echoed text expected, got {other:?}"),
    }
    let job_id = outcome.receipt.receipt.job_id;

    node_loop.abort();
    sigterm_and_wait(&mut coordinator).await;

    // Life 2: same home, a fresh port. The identity holds and the journal
    // answers for the first life's job; the operator registry is empty on
    // purpose, since nodes re-register on their own. The OS assigns life 2
    // whatever port is free, so its clients rebind to the new url.
    let (mut coordinator, url) = spawn_healthy(|port| spawn_coordinator(home.path(), port)).await;
    let config = BuyerConfig {
        coordinator_url: url.clone(),
        ..config
    };
    let reloaded =
        LocalIdentity::load_or_create(&home.path().join("identity.json"), "coordinator@compute")
            .unwrap()
            .agent_id()
            .pubkey_base58();
    assert_eq!(reloaded, pinned, "restart keeps the pinned identity");
    assert_eq!(
        operators_registered(&url).await,
        0,
        "registrations are deliberately not journaled"
    );

    let rows = list_verified_jobs(&http, &config, &buyer, 20)
        .await
        .unwrap();
    let row = rows
        .iter()
        .find(|r| r.job_id == job_id)
        .expect("the journal replays the settled job");
    assert_eq!(row.status, "completed");
    assert_eq!(row.receipt_verified, Some(true));
    assert_eq!(row.price_micro_usdc, 25_000);

    // The market lives on: a re-registered node serves a second job.
    let node_loop = serve_echo_node(&url, &pinned).await;
    let second = dispatch_and_verify(
        &http,
        &config,
        &buyer,
        JobRequest {
            kind: JobKind::InferenceCall,
            input: vec![Content::text("the second life question")],
            model: None,
            gpu_class: None,
            min_vram_gb: None,
            min_reputation_bps: None,
            price_micro_usdc: 25_000,
            deadline_ms: 30_000,
        },
    )
    .await
    .expect("the restarted coordinator serves new work");
    assert_ne!(second.receipt.receipt.job_id, job_id);

    node_loop.abort();
    sigterm_and_wait(&mut coordinator).await;
}

/// A half-configured deposit rail must never boot: verifying deposits
/// against the wrong account is a money bug, so the binary dies loud
/// before binding.
#[tokio::test]
async fn a_half_configured_deposit_rail_fails_the_boot() {
    let home = tempfile::tempdir().unwrap();
    let port = free_port().await;
    let out = Command::new(env!("CARGO_BIN_EXE_covenant-compute-coordinator"))
        .env_clear()
        .env("COVENANT_COMPUTE_COORDINATOR_HOME", home.path())
        .env("COVENANT_COMPUTE_COORDINATOR_PORT", port.to_string())
        .env("COVENANT_COMPUTE_RAIL_RPC_URL", "http://127.0.0.1:1")
        .output()
        .await
        .unwrap();
    assert!(
        !out.status.success(),
        "a half-configured rail must not boot"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("must be set together"), "got: {stderr}");
}

/// Redundancy sampling can only fault a divergent operator through a
/// strict majority, which needs the source plus at least two mirrors. A
/// single mirror still spends bootstrap subsidy on every sample yet can
/// never adjudicate one, so enabling sampling with fewer than two mirrors
/// is refused at boot rather than run as a silent, paid no-op.
#[tokio::test]
async fn redundancy_sampling_with_too_few_mirrors_fails_the_boot() {
    let home = tempfile::tempdir().unwrap();
    let port = free_port().await;
    let out = Command::new(env!("CARGO_BIN_EXE_covenant-compute-coordinator"))
        .env_clear()
        .env("COVENANT_COMPUTE_COORDINATOR_HOME", home.path())
        .env("COVENANT_COMPUTE_COORDINATOR_PORT", port.to_string())
        .env("COVENANT_COMPUTE_REDUNDANCY_INTERVAL_SECS", "1")
        .env("COVENANT_COMPUTE_REDUNDANCY_MIRRORS", "1")
        .output()
        .await
        .unwrap();
    assert!(
        !out.status.success(),
        "redundancy sampling with one mirror must not boot"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("must be at least 2"), "got: {stderr}");
}

/// A canary probe whose deadline is too short to deliver and serve expires
/// on arrival, and a deadline-expired canary faults every honest operator
/// it touches. A zero (or sub-second) deadline is therefore refused at boot
/// rather than left to silently degrade the operators it was meant to vet.
#[tokio::test]
async fn canary_probing_with_a_sub_second_deadline_fails_the_boot() {
    let home = tempfile::tempdir().unwrap();
    let port = free_port().await;
    let out = Command::new(env!("CARGO_BIN_EXE_covenant-compute-coordinator"))
        .env_clear()
        .env("COVENANT_COMPUTE_COORDINATOR_HOME", home.path())
        .env("COVENANT_COMPUTE_COORDINATOR_PORT", port.to_string())
        .env("COVENANT_COMPUTE_CANARY_INTERVAL_SECS", "1")
        .env("COVENANT_COMPUTE_CANARY_DEADLINE_MS", "0")
        .output()
        .await
        .unwrap();
    assert!(
        !out.status.success(),
        "canary probing with a zero deadline must not boot"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("must be at least 1000"), "got: {stderr}");
}

/// The admin surface's whole auth posture rides one env knob through
/// the binary: with COVENANT_COMPUTE_ADMIN_TOKEN set, only the exact
/// bearer clears the gate; without it, the surface stays closed to
/// every caller. The mark-paid body is well-formed throughout, so the
/// only thing deciding each verdict is the token.
#[tokio::test]
async fn the_admin_token_env_knob_gates_the_spawned_binarys_admin_surface() {
    let home = tempfile::tempdir().unwrap();
    let (mut coordinator, url) = spawn_healthy(|port| {
        Command::new(env!("CARGO_BIN_EXE_covenant-compute-coordinator"))
            .env_clear()
            .env("COVENANT_COMPUTE_COORDINATOR_HOME", home.path())
            .env("COVENANT_COMPUTE_COORDINATOR_PORT", port.to_string())
            .env("COVENANT_COMPUTE_LONG_POLL_SECS", "1")
            .env("COVENANT_COMPUTE_ADMIN_TOKEN", "the-operator-secret")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn covenant-compute-coordinator")
    })
    .await;

    let http = reqwest::Client::new();
    let mark_paid = format!("{url}/federation/partners/nobody/payouts");
    let body = serde_json::json!({ "amount_micro_usdc": 1_000, "reference": "tx-reference" });

    let bare = http.post(&mark_paid).json(&body).send().await.unwrap();
    assert_eq!(bare.status(), 401, "no bearer, no admin");

    let wrong = http
        .post(&mark_paid)
        .bearer_auth("not-the-secret")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);
    assert!(wrong.text().await.unwrap().contains("admin token rejected"));

    // The right bearer clears the gate — this deployment simply has no
    // accruals under the code, which is the NEXT check's verdict.
    let right = http
        .post(&mark_paid)
        .bearer_auth("the-operator-secret")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(right.status(), 404);
    assert!(right
        .text()
        .await
        .unwrap()
        .contains("no accruals under code nobody"));

    sigterm_and_wait(&mut coordinator).await;

    // Without the env knob the surface is closed to every bearer —
    // fail-closed is the default, not a configuration.
    let home = tempfile::tempdir().unwrap();
    let (mut coordinator, url) = spawn_healthy(|port| spawn_coordinator(home.path(), port)).await;
    let closed = http
        .post(format!("{url}/federation/partners/nobody/payouts"))
        .bearer_auth("the-operator-secret")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(closed.status(), 401);
    assert!(closed
        .text()
        .await
        .unwrap()
        .contains("no admin token is configured"));
    sigterm_and_wait(&mut coordinator).await;
}

/// C9 volumetric backstops, proven through the spawned process's own
/// environment: the two caps a coordinator fronting open traffic
/// directly relies on. `COVENANT_COMPUTE_MAX_OPERATORS` bounds what a
/// registration flood can allocate — a stranger past the cap gets 503,
/// but a KNOWN operator coming back always lands (a restart must never
/// be locked out by the flood it isn't part of).
/// `COVENANT_COMPUTE_MAX_INFLIGHT_PER_BUYER` bounds unpriced holds — a
/// second live job from the same buyer gets 429, and the ceiling is a
/// live gauge, not a lifetime tally: once the first concludes, the
/// buyer buys again. Both knobs are read only in `main.rs`, so nothing
/// but a spawned process exercises the parse-and-wire path.
#[tokio::test]
async fn the_volumetric_cap_env_knobs_bound_registrations_and_in_flight_holds() {
    let home = tempfile::tempdir().unwrap();
    let (mut coordinator, url) = spawn_healthy(|port| {
        Command::new(env!("CARGO_BIN_EXE_covenant-compute-coordinator"))
            .env_clear()
            .env("COVENANT_COMPUTE_COORDINATOR_HOME", home.path())
            .env("COVENANT_COMPUTE_COORDINATOR_PORT", port.to_string())
            .env("COVENANT_COMPUTE_LONG_POLL_SECS", "1")
            .env("COVENANT_COMPUTE_MAX_OPERATORS", "1")
            .env("COVENANT_COMPUTE_MAX_INFLIGHT_PER_BUYER", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn covenant-compute-coordinator")
    })
    .await;

    let http = reqwest::Client::new();
    let register_url = format!("{url}/federation/operators/register");
    let coordinator_pubkey =
        LocalIdentity::load_or_create(&home.path().join("identity.json"), "coordinator@compute")
            .unwrap()
            .agent_id()
            .pubkey_base58();

    // The one operator the cap admits, serving inference.
    let operator = LocalIdentity::generate("operator@caps");
    let profile = CapabilityProfile {
        operator: operator.agent_id(),
        hardware: HardwareClass::CpuOnly,
        vram_gb: 0,
        models_served: vec!["any".into()],
        job_kinds: vec![JobKind::InferenceCall],
        price: PriceAsk {
            unit: PriceUnit::PerJob,
            micro_usdc: 10_000,
        },
        tee_capable: false,
        kind_prices: Vec::new(),
        kind_models: Vec::new(),
    };
    let admitted = http
        .post(&register_url)
        .json(&RegisterRequest::sign(profile.clone(), payout_addr(2), &operator).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(admitted.status(), 200, "the first operator fills the cap");

    // A second, different operator is growth past the cap: 503, named.
    let stranger = LocalIdentity::generate("stranger@caps");
    let stranger_profile = CapabilityProfile {
        operator: stranger.agent_id(),
        ..profile.clone()
    };
    let refused = http
        .post(&register_url)
        .json(&RegisterRequest::sign(stranger_profile, payout_addr(4), &stranger).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 503, "a stranger past the cap is refused");
    assert!(refused
        .text()
        .await
        .unwrap()
        .contains("at its capacity of 1"));

    // The known operator re-registering is not growth — it always
    // lands. This registration runs through the node's own client so
    // the session it mints is the one that serves the held job below;
    // under the cap it is the returning-operator path, not a new slot.
    let client = Arc::new(HttpCoordinatorClient::with_config(
        url.clone(),
        Duration::from_secs(5),
        2,
    ));
    let returning = client
        .register(RegisterRequest::sign(profile.clone(), payout_addr(2), &operator).unwrap())
        .await
        .expect("a known operator coming back is never locked out by the cap");
    assert!(returning.accepted);

    // In-flight ceiling: submit one job that matches the operator but
    // leave it unserved (no node loop yet) so it sits Offered — one in
    // flight. A raw POST is deliberate: the buyer client would retry a
    // 429, and here the 429 is the verdict under test.
    let buyer = LocalIdentity::generate("buyer@caps");
    let config = BuyerConfig {
        coordinator_url: url.clone(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    let job = |tag: &str| JobRequest {
        kind: JobKind::InferenceCall,
        input: vec![Content::text(tag)],
        model: None,
        gpu_class: None,
        min_vram_gb: None,
        min_reputation_bps: None,
        price_micro_usdc: 25_000,
        deadline_ms: 30_000,
    };
    let first = sign_envelope(&config, &buyer, job("caps-first")).unwrap();
    let submit_url = format!("{url}/federation/jobs");
    let held = http.post(&submit_url).json(&first).send().await.unwrap();
    assert_eq!(held.status(), 202, "the first job is held and offered");

    let second = sign_envelope(&config, &buyer, job("caps-second")).unwrap();
    let over_ceiling = http.post(&submit_url).json(&second).send().await.unwrap();
    assert_eq!(
        over_ceiling.status(),
        429,
        "a second live job from the same buyer is over the ceiling"
    );
    assert!(over_ceiling.text().await.unwrap().contains("ceiling 1"));

    // Serve the held job with the admitted operator (the only one the
    // cap allows), draining the buyer's in-flight count back to zero.
    let node = Node::new(
        operator,
        profile,
        client,
        Arc::new(EchoExecutor),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(covenant_audit::InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58: coordinator_pubkey,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(1),
            fee_bps: 0,
        },
    );
    let served = node
        .run_once()
        .await
        .expect("serve the held job")
        .expect("the held job is offered to the only operator");
    assert!(served.error_message.is_none());
    served
        .receipt
        .verify()
        .expect("the served receipt verifies");

    // The ceiling is a live gauge, not a lifetime tally: with the first
    // job concluded the buyer's in-flight count is back to zero, so a
    // fresh submission is held again (202) — the same submission that
    // was refused 429 a moment ago while the first job sat Offered.
    let third = sign_envelope(&config, &buyer, job("caps-third")).unwrap();
    let freed = http.post(&submit_url).json(&third).send().await.unwrap();
    assert_eq!(
        freed.status(),
        202,
        "the concluded job freed the ceiling for the buyer's next"
    );

    sigterm_and_wait(&mut coordinator).await;
}

/// The wire-version floor knob, end to end through the real binary:
/// `COVENANT_COMPUTE_MIN_PROTOCOL=1` refuses a versionless
/// `/federation/*` call 426 while a client declaring the current
/// protocol passes — and `/health` stays open, or the deploy's own
/// probe would mark a correctly-floored coordinator unhealthy.
#[tokio::test]
async fn the_wire_version_floor_env_knob_refuses_versionless_clients() {
    let home = tempfile::tempdir().unwrap();
    let (mut coordinator, url) = spawn_healthy(|port| {
        Command::new(env!("CARGO_BIN_EXE_covenant-compute-coordinator"))
            .env_clear()
            .env("COVENANT_COMPUTE_COORDINATOR_HOME", home.path())
            .env("COVENANT_COMPUTE_COORDINATOR_PORT", port.to_string())
            .env("COVENANT_COMPUTE_LONG_POLL_SECS", "1")
            .env("COVENANT_COMPUTE_MIN_PROTOCOL", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn covenant-compute-coordinator")
    })
    .await;

    let http = reqwest::Client::new();
    let capacity_url = format!("{url}/federation/capacity");
    let versionless = http.get(&capacity_url).send().await.unwrap();
    assert_eq!(versionless.status(), 426, "version 0 sits below the floor");
    let body: serde_json::Value = versionless.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("upgrade"),
        "the refusal tells the operator what to do: {body}"
    );

    let current = http
        .get(&capacity_url)
        .header(
            covenant_compute_protocol::PROTOCOL_VERSION_HEADER,
            covenant_compute_protocol::PROTOCOL_VERSION.to_string(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(current.status(), 200, "a current client clears the floor");

    sigterm_and_wait(&mut coordinator).await;
}

/// Spawn like [`spawn_coordinator`] but with an explicit dispute
/// window, the C4 knob under test. `FEE_BPS` stays at 250 so
/// [`serve_echo_node`]'s disclosure assertion holds.
fn spawn_coordinator_dispute_window(home: &std::path::Path, port: u16, window_secs: u64) -> Child {
    Command::new(env!("CARGO_BIN_EXE_covenant-compute-coordinator"))
        .env_clear()
        .env("COVENANT_COMPUTE_COORDINATOR_HOME", home)
        .env("COVENANT_COMPUTE_COORDINATOR_PORT", port.to_string())
        .env("COVENANT_COMPUTE_LONG_POLL_SECS", "1")
        .env("COVENANT_COMPUTE_FEE_BPS", "250")
        .env(
            "COVENANT_COMPUTE_DISPUTE_WINDOW_SECS",
            window_secs.to_string(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenant-compute-coordinator")
}

/// Serve one paid echo job to conclusion and return its job id, ready
/// to dispute.
async fn serve_one_completed_job(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer: &LocalIdentity,
    tag: &str,
) -> Uuid {
    let outcome = dispatch_and_verify(
        http,
        config,
        buyer,
        JobRequest {
            kind: JobKind::InferenceCall,
            input: vec![Content::text(tag)],
            model: None,
            gpu_class: None,
            min_vram_gb: None,
            min_reputation_bps: None,
            price_micro_usdc: 25_000,
            deadline_ms: 30_000,
        },
    )
    .await
    .expect("the job settles before it can be disputed");
    outcome.receipt.receipt.job_id
}

/// C4 dispute window, proven through the spawned process: the knob
/// decides whether a buyer's post-conclusion dispute lands. At zero
/// every dispute is refused with the window-closed wording (a
/// deployment that has turned disputes off); at the default 24h a
/// dispute of a just-concluded job lands and marks the record. The
/// knob is read only in `main.rs`, so only a spawned process exercises
/// it end to end.
#[tokio::test]
async fn the_dispute_window_env_knob_decides_whether_a_dispute_lands() {
    let http = reqwest::Client::new();

    // Life 1: the window is closed (0s) — a concluded job's dispute is
    // refused, naming the window.
    let home = tempfile::tempdir().unwrap();
    let (mut coordinator, url) =
        spawn_healthy(|port| spawn_coordinator_dispute_window(home.path(), port, 0)).await;
    let pinned =
        LocalIdentity::load_or_create(&home.path().join("identity.json"), "coordinator@compute")
            .unwrap()
            .agent_id()
            .pubkey_base58();
    let node_loop = serve_echo_node(&url, &pinned).await;

    let config = BuyerConfig {
        coordinator_url: url.clone(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    let buyer = LocalIdentity::generate("buyer@dispute");
    let job_id = serve_one_completed_job(&http, &config, &buyer, "dispute-window-closed").await;

    let refused = covenant_compute_buyer::dispute_job(
        &http,
        &config,
        &buyer,
        job_id,
        "the output was wrong".into(),
    )
    .await
    .expect_err("a zero window refuses every dispute");
    let refused = refused.to_string();
    assert!(
        refused.contains("409") && refused.contains("has closed"),
        "the refusal names the closed window, got: {refused}"
    );

    node_loop.abort();
    sigterm_and_wait(&mut coordinator).await;

    // Life 2: the default window is open — the same dispatch-then-
    // dispute now marks the record instead of bouncing.
    let home = tempfile::tempdir().unwrap();
    let (mut coordinator, url) =
        spawn_healthy(|port| spawn_coordinator_dispute_window(home.path(), port, 24 * 60 * 60))
            .await;
    let pinned =
        LocalIdentity::load_or_create(&home.path().join("identity.json"), "coordinator@compute")
            .unwrap()
            .agent_id()
            .pubkey_base58();
    let node_loop = serve_echo_node(&url, &pinned).await;

    let config = BuyerConfig {
        coordinator_url: url.clone(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    let buyer = LocalIdentity::generate("buyer@dispute");
    let job_id = serve_one_completed_job(&http, &config, &buyer, "dispute-window-open").await;

    let landed = covenant_compute_buyer::dispute_job(
        &http,
        &config,
        &buyer,
        job_id,
        "the output was wrong".into(),
    )
    .await
    .expect("an open window accepts the buyer's dispute");
    assert_eq!(landed.job_id, job_id);
    assert!(landed.disputed, "the record is marked disputed");

    node_loop.abort();
    sigterm_and_wait(&mut coordinator).await;
}

/// The packaged binary's first-contact contract: `--help` and
/// `--version` answer and exit — no identity minted, no journal, no
/// home directory, no server — and any unexpected argument refuses
/// rather than silently booting a misconfigured service.
#[tokio::test]
async fn help_version_and_unexpected_arguments_answer_without_booting() {
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().join("never-created");
    let bare = |arg: &str| {
        Command::new(env!("CARGO_BIN_EXE_covenant-compute-coordinator"))
            .env_clear()
            .env("COVENANT_COMPUTE_COORDINATOR_HOME", &home)
            .arg(arg)
            .output()
    };

    let help = bare("--help").await.unwrap();
    assert!(help.status.success(), "--help exits 0");
    let text = String::from_utf8_lossy(&help.stdout);
    for token in [
        "COVENANT_COMPUTE_FUNDING_SOURCE",
        "COVENANT_COMPUTE_REQUIRE_PREFUNDED",
        "README",
    ] {
        assert!(text.contains(token), "usage names {token}: {text}");
    }
    assert!(
        !home.exists(),
        "asking for help must not create the coordinator home"
    );

    let version = bare("--version").await.unwrap();
    assert!(version.status.success(), "--version exits 0");
    assert_eq!(
        String::from_utf8_lossy(&version.stdout),
        format!(
            "covenant-compute-coordinator {}\n",
            env!("CARGO_PKG_VERSION")
        ),
    );

    let unexpected = bare("--port").await.unwrap();
    assert!(
        !unexpected.status.success(),
        "an unexpected argument refuses instead of booting"
    );
    let err = String::from_utf8_lossy(&unexpected.stderr);
    assert!(
        err.contains("unexpected argument") && err.contains("--help"),
        "{err}"
    );
    assert!(!home.exists(), "a refused boot leaves no home behind");
}

/// A runtime subsidy close outlives a restart of the real binary whose
/// environment still supplies the policy — the deployed shape of the
/// silent-re-arm hazard, since env files outlive admin actions. Life 1
/// boots armed and is closed over the admin surface; life 2 boots from
/// the same home with the same subsidy vars and must come up shut.
#[tokio::test]
async fn a_runtime_subsidy_close_outlives_a_restart_that_still_carries_the_env_policy() {
    let home = tempfile::tempdir().unwrap();
    let spawn_armed = |port: u16| {
        Command::new(env!("CARGO_BIN_EXE_covenant-compute-coordinator"))
            .env_clear()
            .env("COVENANT_COMPUTE_COORDINATOR_HOME", home.path())
            .env("COVENANT_COMPUTE_COORDINATOR_PORT", port.to_string())
            .env("COVENANT_COMPUTE_LONG_POLL_SECS", "1")
            .env("COVENANT_COMPUTE_SUBSIDY_MAX_RATIO_BPS", "10000")
            .env("COVENANT_COMPUTE_SUBSIDY_FLOOR_MICRO_USDC", "1000000")
            .env("COVENANT_COMPUTE_ADMIN_TOKEN", "the-operator-secret")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn covenant-compute-coordinator")
    };
    let http = reqwest::Client::new();
    let subsidy_view = |http: reqwest::Client, url: String| async move {
        http.get(format!("{url}/federation/subsidy"))
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap()
    };

    let (mut coordinator, url) = spawn_healthy(&spawn_armed).await;
    let view = subsidy_view(http.clone(), url.clone()).await;
    assert_eq!(view["enforced"], true, "life 1 boots armed: {view}");
    assert_eq!(view["closed"], false);

    let closed: serde_json::Value = http
        .post(format!("{url}/federation/subsidy/close"))
        .bearer_auth("the-operator-secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(closed["enforced"], false);
    assert_eq!(closed["closed"], true);

    sigterm_and_wait(&mut coordinator).await;

    // Life 2: same home, a fresh port; the policy vars are still set, and
    // the journaled close must outrank them. Held to end of scope so
    // kill_on_drop stops it once the reads below have run.
    let (_coordinator, url) = spawn_healthy(&spawn_armed).await;
    let view = subsidy_view(http.clone(), url.clone()).await;
    assert_eq!(
        view["enforced"], false,
        "the env policy must not re-arm a closed switch: {view}"
    );
    assert_eq!(view["closed"], true);
    let metrics = http
        .get(format!("{url}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(metrics.contains("compute_subsidy_closed 1"), "{metrics}");
}
