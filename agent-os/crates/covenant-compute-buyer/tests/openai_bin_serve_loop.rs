//! The `covenant-compute-openai` binary end to end: a real spawned
//! process, configured only through its environment, standing in front of
//! a live coordinator and a serving node. Proves the deployed OpenAI front
//! door — the one an external OpenAI-SDK buyer actually points a client at
//! — loads its persisted identity, funds a buy from that identity's
//! coordinator balance, and answers a plain `POST /v1/chat/completions`
//! with a standard `chat.completion` carrying the operator's verified
//! receipt. The process-level twin of `openai_endpoint_loop.rs`, whose
//! router is exercised in-process; this is the only buyer binary that was
//! never spawned.

#![cfg(unix)]

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use covenant_compute_buyer::{claim_deposit, http_client, BuyerConfig};
use covenant_compute_coordinator::{
    router as compute_router, CoordinatorConfig, CoordinatorState, MockPayout, MockRail,
    NoReputation, VerifiedDeposit,
};
use covenant_compute_node::{
    ChunkSink, Coordinator as _, ExecutionOutcome, ExecutorError, HttpCoordinatorClient,
    InMemoryEarningsLedger, JobExecutor, Node, NodeConfig, OllamaExecutor, DEFAULT_OLLAMA_URL,
};
use covenant_compute_protocol::{
    CapabilityProfile, FundingSource, HardwareClass, JobEnvelopePayload, JobKind, PriceAsk,
    PriceUnit, RegisterRequest,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use serde_json::{json, Value};
use tokio::process::{Child, Command};

const SERVED_MODEL: &str = "qwen2.5:0.5b";

/// Answers a chat job with a fixed assistant reply and real token counts,
/// so the endpoint's usage is meaningful. An echo executor would hand back
/// the packed chat JSON, which carries no assistant text.
#[derive(Clone, Copy)]
struct ReplyExecutor;

#[async_trait]
impl JobExecutor for ReplyExecutor {
    async fn execute(
        &self,
        _job: &JobEnvelopePayload,
        _deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        Ok(ExecutionOutcome {
            output: vec![Content::text("the assistant reply")],
            wall_ms: 1,
            tokens_in: Some(7),
            tokens_out: Some(3),
            finish_reason: None,
        })
    }

    async fn execute_streaming(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
        sink: ChunkSink,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let _ = sink.send("the assistant reply".to_string()).await;
        self.execute(job, deadline).await
    }
}

/// Answers like [`ReplyExecutor`] but signals the moment it begins and then
/// dwells, so a test can hold a buy provably in flight while it signals the
/// front door, rather than racing a fixed sleep.
#[derive(Clone)]
struct SlowExecutor {
    started: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl JobExecutor for SlowExecutor {
    async fn execute(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        self.started.notify_one();
        tokio::time::sleep(Duration::from_secs(2)).await;
        ReplyExecutor.execute(job, deadline).await
    }

    async fn execute_streaming(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
        sink: ChunkSink,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        self.started.notify_one();
        tokio::time::sleep(Duration::from_secs(2)).await;
        ReplyExecutor.execute_streaming(job, deadline, sink).await
    }
}

/// A live market: an in-process coordinator with a claimable deposit rail
/// plus one registered node serving [`SERVED_MODEL`]. The coordinator runs
/// in-process because its deposit rail is a [`MockRail`] that has to be
/// injected — the deployed rail reads Solana, which a hermetic test can't.
/// The binary under test is the front door, and it dials this coordinator
/// over loopback exactly as it would a deployed one.
struct Market {
    url: String,
    rail: Arc<MockRail>,
    _coordinator_home: tempfile::TempDir,
}

impl Market {
    async fn launch<X: JobExecutor + 'static>(executor: X) -> Self {
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

        let operator_identity = LocalIdentity::generate("operator@test");
        let profile = CapabilityProfile {
            operator: operator_identity.agent_id(),
            hardware: HardwareClass::CpuOnly,
            vram_gb: 0,
            models_served: vec![SERVED_MODEL.into()],
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
            url.clone(),
            Duration::from_secs(5),
            2,
        ));
        client
            .register(
                RegisterRequest::sign(profile.clone(), payout_addr(2), &operator_identity).unwrap(),
            )
            .await
            .unwrap();
        let node = Node::new(
            operator_identity,
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

        Self {
            url,
            rail,
            _coordinator_home: coordinator_home,
        }
    }
}

fn payout_addr(seed: u8) -> String {
    bs58::encode([seed; 32]).into_string()
}

/// A home the binary boots from with a funded buyer identity already in
/// place: the same `identity.json` the binary loads, whose coordinator
/// balance a claimed deposit has topped up. The binary funds no deposit
/// itself, so a buy only clears because this ran first.
async fn funded_home(market: &Market, amount_micro_usdc: u64) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let identity =
        LocalIdentity::load_or_create(&home.path().join("identity.json"), "buyer@compute").unwrap();
    market.rail.preload(VerifiedDeposit {
        deposit_id: "openai-bin-deposit".into(),
        buyer_pubkey_b58: identity.agent_id().pubkey_base58(),
        amount_micro_usdc,
    });
    let config = BuyerConfig {
        coordinator_url: market.url.clone(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    claim_deposit(&http_client(), &config, &identity, "openai-bin-deposit")
        .await
        .expect("the buyer funds itself before the binary serves");
    home
}

async fn free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

/// The deployed invocation: no arguments, everything through the
/// environment, the way an operator would run the front door.
fn spawn_openai(home: &std::path::Path, coordinator_url: &str, port: u16, api_key: &str) -> Child {
    Command::new(env!("CARGO_BIN_EXE_covenant-compute-openai"))
        .env_clear()
        .env("COVENANT_COMPUTE_COORDINATOR_URL", coordinator_url)
        .env("COVENANT_COMPUTE_OPENAI_BIND", format!("127.0.0.1:{port}"))
        .env("COVENANT_COMPUTE_OPENAI_HOME", home)
        .env("COVENANT_COMPUTE_OPENAI_API_KEY", api_key)
        .env("COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC", "1000000")
        .env("COVENANT_COMPUTE_DEADLINE_MS", "30000")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenant-compute-openai")
}

/// The front door lost its port before it served, carrying the exit
/// status. The `free_port` bind-drop-rebind handoff can lose the port to a
/// concurrent suite; a fresh port clears it.
struct DiedBeforeHealthy(String);

/// Polls `/health` (which needs no bearer) until the spawned binary
/// answers, watching the child so a boot failure surfaces as a retry
/// rather than a 120s hang. A parallel full-suite run boots several
/// binaries at once, so a healthy boot can take tens of seconds under load.
async fn wait_healthy(child: &mut Child, base: &str) -> Result<(), DiedBeforeHealthy> {
    let http = reqwest::Client::new();
    for _ in 0..1_200 {
        if let Some(status) = child.try_wait().expect("poll the spawned front door") {
            return Err(DiedBeforeHealthy(status.to_string()));
        }
        if let Ok(resp) = http.get(format!("{base}/health")).send().await {
            if resp.status().is_success() {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the front door booted but never answered /health within 120s");
}

/// Spawn the front door on a free port and wait until it is healthy,
/// retrying past the one flake this harness has: `free_port` hands the
/// child a port it briefly held, so a concurrent suite can grab it in the
/// gap and the child dies at bind. Same identity home across retries, so
/// the funded balance carries over.
async fn spawn_healthy(mut build: impl FnMut(u16) -> Child) -> (Child, String) {
    let mut last_exit = String::new();
    for _ in 0..8 {
        let port = free_port().await;
        let base = format!("http://127.0.0.1:{port}");
        let mut child = build(port);
        match wait_healthy(&mut child, &base).await {
            Ok(()) => return (child, base),
            Err(DiedBeforeHealthy(status)) => last_exit = status,
        }
    }
    panic!("the front door lost its port on 8 successive attempts (last exit: {last_exit})");
}

async fn stop(child: &mut Child) {
    // SIGTERM drains in-flight requests and exits clean, the same posture
    // the coordinator and node binaries take for a supervised stop.
    let pid = child.id().expect("serving child has a pid");
    assert!(std::process::Command::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap()
        .success());
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("the front door drains and exits within 10s of SIGTERM")
        .expect("wait on the front door");
    assert!(status.success(), "a graceful drain exits 0, got {status}");
}

#[tokio::test]
async fn the_openai_binary_serves_a_verified_chat_completion_over_http() {
    let market = Market::launch(ReplyExecutor).await;
    let home = funded_home(&market, 100_000).await;
    let api_key = "sk-covenant-bin-test";
    let (mut child, base) =
        spawn_healthy(|port| spawn_openai(home.path(), &market.url, port, api_key)).await;

    let http = reqwest::Client::new();
    let request = json!({
        "model": SERVED_MODEL,
        "messages": [{ "role": "user", "content": "hello" }],
        "temperature": 0,
    });

    // The bearer the binary read from its environment is enforced: a
    // request without it never reaches the network.
    let unauth = http
        .post(format!("{base}/v1/chat/completions"))
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(unauth.status(), 401);

    // The real buy: a standard chat request, no Covenant-specific fields,
    // the way an OpenAI SDK sends it.
    let resp = http
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(api_key)
        .json(&request)
        .send()
        .await
        .expect("the spawned front door answers");
    assert_eq!(resp.status(), 200, "a funded buy clears through the binary");
    let body: Value = resp.json().await.unwrap();

    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["model"], SERVED_MODEL);
    assert_eq!(
        body["choices"][0]["message"]["content"],
        "the assistant reply"
    );
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
    assert_eq!(body["usage"]["total_tokens"], 10);

    // The receipt the operator signed rode all the way back through the
    // spawned process, and re-verified there.
    assert_eq!(body["covenant"]["receipt_verified"], true);
    assert_eq!(body["covenant"]["price_micro_usdc"], 10_000);
    assert!(body["covenant"]["job_id"].is_string());
    assert!(body["covenant"]["operator_pubkey_b58"].is_string());

    stop(&mut child).await;
}

#[tokio::test]
#[ignore = "requires a running ollama at 127.0.0.1:11434 serving qwen2.5:0.5b"]
async fn the_openai_binary_serves_a_real_backend_chat_completion() {
    // The whole demand front door against a real model: the deployed
    // binary → an in-process coordinator → a serving node → real Ollama →
    // a verified receipt → OpenAI's chat.completion shape. The two hops no
    // other test joins in one path: the actual binary and a real engine.
    let market = Market::launch(OllamaExecutor::new(
        DEFAULT_OLLAMA_URL,
        Some(SERVED_MODEL.into()),
    ))
    .await;
    let home = funded_home(&market, 100_000).await;
    let api_key = "sk-covenant-bin-live";
    let (mut child, base) =
        spawn_healthy(|port| spawn_openai(home.path(), &market.url, port, api_key)).await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(api_key)
        .json(&json!({
            "model": SERVED_MODEL,
            "messages": [{ "role": "user", "content": "Say hello in one short word." }],
            "max_tokens": 16,
            "temperature": 0,
        }))
        .send()
        .await
        .expect("the spawned front door answers");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();

    assert_eq!(body["object"], "chat.completion");
    let content = body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default();
    assert!(
        !content.trim().is_empty(),
        "a real backend returns non-empty completion text: {body}"
    );
    assert!(
        body["usage"]["completion_tokens"].as_u64().unwrap_or(0) > 0,
        "real tokens were metered: {body}"
    );
    assert_eq!(body["covenant"]["receipt_verified"], true);

    stop(&mut child).await;
}

#[tokio::test]
async fn a_sigterm_mid_buy_drains_the_in_flight_request() {
    // The point of the drain, proven rather than asserted on an idle server:
    // a buy already in flight when the signal lands still returns its
    // verified completion to the client who is being charged for it, and the
    // process exits only after.
    let started = Arc::new(tokio::sync::Notify::new());
    let market = Market::launch(SlowExecutor {
        started: started.clone(),
    })
    .await;
    let home = funded_home(&market, 100_000).await;
    let api_key = "sk-covenant-drain";
    let (mut child, base) =
        spawn_healthy(|port| spawn_openai(home.path(), &market.url, port, api_key)).await;

    let call = {
        let base = base.clone();
        let api_key = api_key.to_string();
        tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{base}/v1/chat/completions"))
                .bearer_auth(api_key)
                .json(&json!({
                    "model": SERVED_MODEL,
                    "messages": [{ "role": "user", "content": "hello" }],
                }))
                .send()
                .await
        })
    };

    // Signal only once the node is provably executing the buy, so the
    // request is unambiguously in flight and not a fixed-sleep race.
    tokio::time::timeout(Duration::from_secs(10), started.notified())
        .await
        .expect("the node begins executing the buy before the signal");
    let pid = child.id().expect("serving child has a pid");
    assert!(std::process::Command::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap()
        .success());

    let resp = tokio::time::timeout(Duration::from_secs(15), call)
        .await
        .expect("the drained request returns within 15s")
        .expect("join the request task")
        .expect("the in-flight buy completes through the drain");
    assert_eq!(resp.status(), 200, "the drained buy still serves 200");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["choices"][0]["message"]["content"], "the assistant reply",
        "the completion the client paid for comes back: {body}"
    );
    assert_eq!(body["covenant"]["receipt_verified"], true);

    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("the front door exits after draining")
        .expect("wait on the front door");
    assert!(status.success(), "a drained exit is clean, got {status}");
}
