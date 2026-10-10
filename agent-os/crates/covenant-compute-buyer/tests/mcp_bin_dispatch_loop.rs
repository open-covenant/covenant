//! The `covenant-compute-mcp` binary end to end: a real spawned server
//! process driven over its own stdin/stdout JSON-RPC surface against a
//! real coordinator and a real serving node — the loop an actual MCP
//! client exercises, including a server restart over the same home and
//! a streaming purchase polled to conclusion. The hermetic twin of the
//! manual live-stdio proof.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use covenant_compute_coordinator::{
    router as compute_router, CoordinatorConfig, CoordinatorState, MockPayout, MockRail,
    NoReputation, VerifiedDeposit,
};
use covenant_compute_node::{
    Coordinator as _, EchoExecutor, ExecutionOutcome, ExecutorError, HttpCoordinatorClient,
    InMemoryEarningsLedger, JobExecutor, Node, NodeConfig,
};
use covenant_compute_protocol::{
    speech_output, CapabilityProfile, FundingSource, HardwareClass, JobEnvelopePayload, JobKind,
    PriceAsk, PriceUnit, RegisterRequest,
};
use covenant_identity::LocalIdentity;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    replies: Lines<BufReader<ChildStdout>>,
}

impl Mcp {
    async fn spawn(coordinator_url: &str, home: &std::path::Path) -> Self {
        Self::spawn_with(coordinator_url, home, &[]).await
    }

    async fn spawn_with(
        coordinator_url: &str,
        home: &std::path::Path,
        extra_env: &[(&str, &str)],
    ) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_covenant-compute-mcp"));
        command
            .env_clear()
            .env("COVENANT_COMPUTE_COORDINATOR_URL", coordinator_url)
            .env("COVENANT_COMPUTE_MCP_HOME", home);
        for (key, value) in extra_env {
            command.env(key, value);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn covenant-compute-mcp");
        Self {
            stdin: child.stdin.take().expect("child stdin"),
            replies: BufReader::new(child.stdout.take().expect("child stdout")).lines(),
            child,
        }
    }

    /// Write one message; when it carries an id, read exactly one
    /// stdout line back and require it to be the JSON-RPC reply to
    /// that id. The server loop is sequential, so replies cannot
    /// interleave — any stray line on stdout fails here.
    async fn send(&mut self, msg: Value) -> Option<Value> {
        let mut line = msg.to_string();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await.unwrap();
        self.stdin.flush().await.unwrap();
        let id = msg.get("id")?.clone();
        let reply = tokio::time::timeout(Duration::from_secs(30), self.replies.next_line())
            .await
            .expect("a reply within 30s")
            .expect("read child stdout")
            .expect("child kept stdout open");
        let reply: Value = serde_json::from_str(&reply).expect("every stdout line is JSON-RPC");
        assert_eq!(reply["jsonrpc"], "2.0");
        assert_eq!(reply["id"], id, "the reply answers the request that asked");
        Some(reply)
    }

    async fn call_tool_raw(&mut self, id: u64, name: &str, arguments: Value) -> Value {
        let reply = self
            .send(json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": { "name": name, "arguments": arguments },
            }))
            .await
            .expect("a call has an id");
        reply["result"].clone()
    }

    async fn call_tool(&mut self, id: u64, name: &str, arguments: Value) -> Value {
        let result = self.call_tool_raw(id, name, arguments).await;
        assert_eq!(result["isError"], false, "{name} failed: {result}");
        result
    }

    /// A tool-level refusal: `isError: true` with the reason as the
    /// only content block. Distinct from a JSON-RPC error — the call
    /// itself was well-formed.
    async fn refused(&mut self, id: u64, name: &str, arguments: Value) -> String {
        let result = self.call_tool_raw(id, name, arguments).await;
        assert_eq!(
            result["isError"], true,
            "{name} was expected to refuse: {result}"
        );
        result["content"][0]["text"]
            .as_str()
            .expect("a refusal carries its reason")
            .to_string()
    }

    /// Stdin EOF is the shutdown signal; a healthy server exits 0.
    async fn shutdown(mut self) {
        drop(self.stdin);
        let status = tokio::time::timeout(Duration::from_secs(10), self.child.wait())
            .await
            .expect("exit within 10s of stdin EOF")
            .expect("wait on child");
        assert!(status.success(), "clean exit, got {status}");
    }
}

/// One live market: a coordinator with journaled books and a claimable
/// deposit rail, listening on a local port, plus a registered node
/// serving the echo executor. Tests hold the rig for its url, its rail
/// and the coordinator home's lifetime.
struct Rig {
    url: String,
    rail: Arc<MockRail>,
    _coordinator_home: tempfile::TempDir,
}

impl Rig {
    async fn launch() -> Self {
        Self::launch_with(EchoExecutor, vec![JobKind::InferenceCall]).await
    }

    /// A rig whose node serves speech-synthesis jobs from a fixed mock
    /// clip — the supply side for the `compute.speak` path.
    async fn launch_speaking() -> Self {
        Self::launch_with(SpeakMock, vec![JobKind::SpeechSynthesis]).await
    }

    async fn launch_with<X: JobExecutor + 'static>(executor: X, job_kinds: Vec<JobKind>) -> Self {
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
            models_served: vec!["any".into()],
            job_kinds,
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

/// A payable operator payout address per fixture seed — registration
/// refuses anything that doesn't decode to a 32-byte key.
fn payout_addr(seed: u8) -> String {
    bs58::encode([seed; 32]).into_string()
}

/// Every tool answers text-only blocks for strict-client interop; the
/// last one is a JSON document (the receipt metadata for purchases,
/// the payload for reads).
fn last_block_json(result: &Value) -> Value {
    let blocks = result["content"].as_array().expect("content blocks");
    let last = blocks.last().expect("at least one block");
    assert_eq!(last["type"], "text", "text-only interop");
    serde_json::from_str(last["text"].as_str().expect("text field"))
        .expect("the final block is a JSON document")
}

async fn initialize(mcp: &mut Mcp) {
    let reply = mcp
        .send(json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2024-11-05", "capabilities": {} },
        }))
        .await
        .unwrap();
    assert_eq!(reply["result"]["serverInfo"]["name"], "covenant-compute");
    assert_eq!(reply["result"]["protocolVersion"], "2024-11-05");
    // The initialized notification must consume no reply slot: the
    // next line out of the server answers the ping, not the
    // notification.
    mcp.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
        .await;
    let pong = mcp
        .send(json!({ "jsonrpc": "2.0", "id": 2, "method": "ping" }))
        .await
        .unwrap();
    assert_eq!(pong["result"], json!({}));
}

/// A fixed clip the speech mock returns, so the test asserts the exact
/// bytes reach disk unchanged.
const MOCK_CLIP: &[u8] = b"RIFF\x24\x00\x00\x00WAVEfmt \x10\x00\x00\x00";

/// A node executor that answers a speech job with a fixed clip — the
/// supply half of the `compute.speak` path, deterministic and needing no
/// real synthesizer.
struct SpeakMock;

#[async_trait]
impl JobExecutor for SpeakMock {
    async fn execute(
        &self,
        _job: &JobEnvelopePayload,
        _deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        use base64::Engine as _;
        let audio_base64 = base64::engine::general_purpose::STANDARD.encode(MOCK_CLIP);
        Ok(ExecutionOutcome {
            output: vec![speech_output("say-1", audio_base64, "wav", Some(22_050))],
            wall_ms: 3,
            tokens_in: None,
            tokens_out: None,
            finish_reason: None,
        })
    }
}

/// `compute.speak` end to end through the spawned binary: the clip is
/// saved to disk and the tool result names the file, its size and format,
/// never carrying the base64 audio back into the agent's context.
#[tokio::test]
async fn the_stdio_binary_speaks_and_saves_a_clip_rather_than_returning_base64() {
    let rig = Rig::launch_speaking().await;

    let home = tempfile::tempdir().unwrap();
    let buyer =
        LocalIdentity::load_or_create(&home.path().join("identity.json"), "buyer@compute").unwrap();
    rig.rail.preload(VerifiedDeposit {
        deposit_id: "speak-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });

    let mut mcp = Mcp::spawn(&rig.url, home.path()).await;
    initialize(&mut mcp).await;
    let claimed = last_block_json(
        &mcp.call_tool(
            1,
            "compute.deposit",
            json!({ "deposit_id": "speak-deposit" }),
        )
        .await,
    );
    assert_eq!(claimed["credited"], true);

    let spoken = mcp
        .call_tool(
            2,
            "compute.speak",
            json!({ "text": "covenant compute speaks", "price_micro_usdc": 25_000 }),
        )
        .await;

    // The base64 audio never rides back into the caller's context.
    use base64::Engine as _;
    let audio_b64 = base64::engine::general_purpose::STANDARD.encode(MOCK_CLIP);
    let whole = serde_json::to_string(&spoken).unwrap();
    assert!(
        !whole.contains(&audio_b64),
        "the clip's base64 leaked into the tool result: {whole}"
    );

    // The first block names the saved clip; the file holds the operator's
    // exact bytes, under the server home's clips directory.
    let blocks = spoken["content"].as_array().unwrap();
    let saved: Value = serde_json::from_str(blocks[0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(saved["saved"], true, "{saved}");
    assert_eq!(saved["format"], "wav");
    assert_eq!(saved["model"], "say-1");
    let path = saved["path"].as_str().unwrap();
    assert!(path.ends_with(".wav"), "{path}");
    assert!(
        std::path::Path::new(path).starts_with(home.path()),
        "the clip lands under the server home: {path}"
    );
    assert_eq!(std::fs::read(path).unwrap(), MOCK_CLIP);

    // The verified receipt still follows, so the buyer learns they paid.
    let meta = last_block_json(&spoken);
    assert_eq!(meta["receipt_verified"], true);

    mcp.shutdown().await;
}

#[tokio::test]
async fn the_stdio_binary_buys_replays_and_survives_restart_against_a_real_coordinator() {
    let rig = Rig::launch().await;

    // The server's identity is created before the first spawn so the
    // deposit can name it; the binary loads the same file.
    let home = tempfile::tempdir().unwrap();
    let buyer =
        LocalIdentity::load_or_create(&home.path().join("identity.json"), "buyer@compute").unwrap();
    rig.rail.preload(VerifiedDeposit {
        deposit_id: "bin-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });

    // Life 1: the buyer surface through the real dispatch loop.
    let mut mcp = Mcp::spawn(&rig.url, home.path()).await;
    initialize(&mut mcp).await;

    let listed = mcp
        .send(json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }))
        .await
        .unwrap();
    let tools = listed["result"]["tools"].as_array().unwrap().clone();
    assert_eq!(tools.len(), 19, "the whole buyer surface is advertised");
    assert!(tools.iter().any(|t| t["name"] == "compute.infer"));
    assert!(tools.iter().any(|t| t["name"] == "compute.agent"));
    assert!(tools.iter().any(|t| t["name"] == "compute.fix"));
    assert!(tools.iter().any(|t| t["name"] == "compute.embed"));
    assert!(tools.iter().any(|t| t["name"] == "compute.transcribe"));
    assert!(tools.iter().any(|t| t["name"] == "compute.speak"));
    assert!(tools.iter().any(|t| t["name"] == "compute.output"));
    assert!(tools.iter().any(|t| t["name"] == "compute.withdrawals"));

    // Discovery before spending: the directory shows the rig's one
    // node as a purchasable row — the model id and price floor the
    // purchase below then satisfies, all through the spawned process.
    let market = last_block_json(&mcp.call_tool(35, "compute.capacity", json!({})).await);
    assert_eq!(market["matchable_operators"], 1);
    assert_eq!(market["entries"][0]["kind"], "inference_call");
    assert_eq!(market["entries"][0]["model"], "any");
    assert_eq!(market["entries"][0]["min_ask_micro_usdc"], 10_000);
    assert_eq!(market["entries"][0]["gpu_classes"], json!(["cpu"]));

    let claimed = last_block_json(
        &mcp.call_tool(4, "compute.deposit", json!({"deposit_id": "bin-deposit"}))
            .await,
    );
    assert_eq!(claimed["credited"], true);

    let buy_args = json!({
        "prompt": "the stdio question",
        "price_micro_usdc": 25_000,
        "idempotency_key": "bin-key",
    });
    let bought = mcp.call_tool(5, "compute.infer", buy_args.clone()).await;
    let blocks = bought["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 2, "the echoed output plus the metadata block");
    assert_eq!(blocks[0]["type"], "text");
    assert_eq!(
        blocks[0]["text"], "the stdio question",
        "the output rides ahead of the metadata"
    );
    let meta = last_block_json(&bought);
    assert_eq!(meta["receipt_verified"], true);
    assert_eq!(meta["price_micro_usdc"], 25_000);
    let job_id = meta["job_id"].as_str().expect("job id").to_string();

    let replayed = last_block_json(&mcp.call_tool(6, "compute.infer", buy_args.clone()).await);
    assert_eq!(
        replayed["job_id"].as_str().unwrap(),
        job_id,
        "one key, one purchase — the replay serves the recorded job"
    );

    let receipts = last_block_json(&mcp.call_tool(7, "compute.receipts", json!({})).await);
    assert_eq!(receipts["count"], 1, "the replay bought nothing");
    assert_eq!(receipts["receipts_failing_verification"], 0);
    let row = &receipts["jobs"][0];
    assert_eq!(row["job_id"].as_str().unwrap(), job_id);
    assert_eq!(row["status"], "completed");
    assert!(
        row["payout"].is_object(),
        "the payout block landed on the row"
    );

    // compute.output re-reads the job's output straight from the
    // coordinator, proven locally — the answer survives a lost session,
    // which compute.receipts (hash only) cannot give back.
    let reread = mcp
        .call_tool(9, "compute.output", json!({ "job_id": job_id }))
        .await;
    let reread_blocks = reread["content"].as_array().unwrap();
    assert_eq!(
        reread_blocks[0]["text"], "the stdio question",
        "the output is re-served ahead of the metadata: {reread_blocks:?}"
    );
    let reread_meta = last_block_json(&reread);
    assert_eq!(reread_meta["job_id"].as_str().unwrap(), job_id);
    assert_eq!(reread_meta["receipt_verified"], true);

    let funds = last_block_json(&mcp.call_tool(8, "compute.balance", json!({})).await);
    assert_eq!(funds["balance"]["deposited_micro_usdc"], 100_000);
    assert_eq!(funds["balance"]["charged_micro_usdc"], 25_000);
    assert_eq!(funds["balance"]["available_micro_usdc"], 75_000);

    mcp.shutdown().await;

    // Life 2: same home, fresh process. The journaled key answers the
    // recorded purchase over the wire and the books stand still.
    let mut mcp = Mcp::spawn(&rig.url, home.path()).await;
    initialize(&mut mcp).await;
    let replayed = last_block_json(&mcp.call_tool(3, "compute.infer", buy_args).await);
    assert_eq!(
        replayed["job_id"].as_str().unwrap(),
        job_id,
        "the restarted server serves the recorded purchase, not a second job"
    );
    let receipts = last_block_json(&mcp.call_tool(4, "compute.receipts", json!({})).await);
    assert_eq!(receipts["count"], 1, "no second job across the restart");
    let funds = last_block_json(&mcp.call_tool(5, "compute.balance", json!({})).await);
    assert_eq!(
        funds["balance"]["charged_micro_usdc"], 25_000,
        "no second charge across the restart"
    );
    mcp.shutdown().await;
}

/// compute.infer with no price offered buys at the cheapest matching ask,
/// not the per-call ceiling — the same market-rate default the human CLI
/// takes, since settlement charges the envelope price.
#[tokio::test]
async fn a_default_priced_infer_offers_the_market_rate_not_the_ceiling() {
    let rig = Rig::launch().await;
    let home = tempfile::tempdir().unwrap();
    let buyer =
        LocalIdentity::load_or_create(&home.path().join("identity.json"), "buyer@compute").unwrap();
    rig.rail.preload(VerifiedDeposit {
        deposit_id: "market-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });
    let mut mcp = Mcp::spawn(&rig.url, home.path()).await;
    initialize(&mut mcp).await;
    mcp.call_tool(
        1,
        "compute.deposit",
        json!({"deposit_id": "market-deposit"}),
    )
    .await;

    // The rig node asks 10_000; a default-priced buy offers exactly that,
    // not the $1 ceiling.
    let bought = last_block_json(
        &mcp.call_tool(2, "compute.infer", json!({ "prompt": "at market" }))
            .await,
    );
    assert_eq!(bought["receipt_verified"], true);
    assert_eq!(
        bought["price_micro_usdc"], 10_000,
        "the default offer is the 10000 ask, not the ceiling: {bought}"
    );

    let funds = last_block_json(&mcp.call_tool(3, "compute.balance", json!({})).await);
    assert_eq!(funds["balance"]["charged_micro_usdc"], 10_000);
    mcp.shutdown().await;
}

/// A `dry_run` on the agent surface resolves the price and routing a real
/// buy would use and returns them without dispatching or spending —
/// including for a buyer that has deposited nothing, since the preview
/// never reads funds. It refuses an unservable ask up front (no ceiling
/// fallback), and refuses the flags that only make sense once a purchase
/// is real (a key to reserve, a stream to open).
#[tokio::test]
async fn a_dry_run_previews_the_price_and_spends_nothing() {
    let rig = Rig::launch().await;
    let home = tempfile::tempdir().unwrap();
    let buyer =
        LocalIdentity::load_or_create(&home.path().join("identity.json"), "buyer@compute").unwrap();
    rig.rail.preload(VerifiedDeposit {
        deposit_id: "preview-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });
    let mut mcp = Mcp::spawn(&rig.url, home.path()).await;
    initialize(&mut mcp).await;

    // Before any deposit is claimed: the preview resolves the market and
    // reports the 10_000 cheapest matching ask, tagged as a dry run, with
    // no balance to read.
    let preview = last_block_json(
        &mcp.call_tool(
            1,
            "compute.infer",
            json!({ "prompt": "preview me", "dry_run": true }),
        )
        .await,
    );
    assert_eq!(preview["dry_run"], true, "tagged a dry run: {preview}");
    assert_eq!(preview["kind"], "inference_call");
    assert_eq!(preview["price_micro_usdc"], 10_000);
    assert_eq!(preview["price_source"], "cheapest_matching_ask");

    // An explicit price previews as the buyer's own figure.
    let explicit = last_block_json(
        &mcp.call_tool(
            2,
            "compute.infer",
            json!({ "prompt": "q", "price_micro_usdc": 20_000, "dry_run": true }),
        )
        .await,
    );
    assert_eq!(explicit["price_micro_usdc"], 20_000);
    assert_eq!(explicit["price_source"], "explicit");

    // The batch tool previews too. The rig serves only inference, so an
    // explicit price shows the batch path resolving with no batch operator
    // to quote from.
    let batch = last_block_json(
        &mcp.call_tool(
            3,
            "compute.run",
            json!({ "command": "echo hi", "price_micro_usdc": 15_000, "dry_run": true }),
        )
        .await,
    );
    assert_eq!(batch["kind"], "batch_job");
    assert_eq!(batch["price_micro_usdc"], 15_000);
    assert_eq!(batch["price_source"], "explicit");

    // An ask no operator can serve refuses in preview with the same "no
    // operator is serving" verdict a real buy resolves to — not a silent
    // fallback to the ceiling — having dispatched nothing.
    let unservable = mcp
        .refused(
            4,
            "compute.infer",
            json!({ "prompt": "q", "min_vram_gb": 999, "dry_run": true }),
        )
        .await;
    assert!(
        unservable.contains("no operator is serving")
            && unservable.contains("nothing was dispatched"),
        "an impossible ask refuses in preview: {unservable}"
    );

    // A preview reserves no purchase and opens no feed, so it refuses the
    // key and the stream, each naming what to drop.
    let with_key = mcp
        .refused(
            5,
            "compute.infer",
            json!({ "prompt": "q", "idempotency_key": "k", "dry_run": true }),
        )
        .await;
    assert!(
        with_key.contains("idempotency_key") && with_key.contains("dry run"),
        "dry_run + key is refused, naming both: {with_key}"
    );
    let with_stream = mcp
        .refused(
            6,
            "compute.stream_start",
            json!({ "prompt": "q", "dry_run": true }),
        )
        .await;
    assert!(
        with_stream.contains("dry_run") && with_stream.contains("compute.infer"),
        "a stream has nothing to preview; it points at compute.infer: {with_stream}"
    );

    // The contract: every one of those previews moved no money. Claim the
    // deposit and the balance shows the full amount available, nothing
    // charged.
    mcp.call_tool(
        7,
        "compute.deposit",
        json!({"deposit_id": "preview-deposit"}),
    )
    .await;
    let funds = last_block_json(&mcp.call_tool(8, "compute.balance", json!({})).await);
    assert_eq!(
        funds["balance"]["charged_micro_usdc"], 0,
        "no preview charged anything: {funds}"
    );
    assert_eq!(funds["balance"]["available_micro_usdc"], 100_000);
    mcp.shutdown().await;
}

#[tokio::test]
async fn the_stdio_binary_streams_a_purchase_and_polls_the_feed_to_conclusion() {
    let rig = Rig::launch().await;

    let home = tempfile::tempdir().unwrap();
    let buyer =
        LocalIdentity::load_or_create(&home.path().join("identity.json"), "buyer@compute").unwrap();
    rig.rail.preload(VerifiedDeposit {
        deposit_id: "stream-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });

    let mut mcp = Mcp::spawn(&rig.url, home.path()).await;
    initialize(&mut mcp).await;
    let claimed = last_block_json(
        &mcp.call_tool(
            3,
            "compute.deposit",
            json!({"deposit_id": "stream-deposit"}),
        )
        .await,
    );
    assert_eq!(claimed["credited"], true);

    // The start returns as soon as the coordinator accepts the
    // envelope; the server's background drain relays the feed.
    let started = last_block_json(
        &mcp.call_tool(
            4,
            "compute.stream_start",
            json!({ "prompt": "the streamed question", "price_micro_usdc": 25_000 }),
        )
        .await,
    );
    assert_eq!(started["status"], "streaming");
    assert_eq!(started["next_seq"], 0);
    assert_eq!(started["price_micro_usdc"], 25_000);
    assert_eq!(started["poll_tool"], "compute.stream_poll");
    let job_id = started["job_id"].as_str().expect("job id").to_string();

    // Cursor reads until the drain concludes. Chunks count whichever
    // poll they land on; the cursor may only move forward.
    let mut assembled = String::new();
    let mut since = 0;
    let mut id = 5;
    let mut polls = 0;
    let concluding = loop {
        polls += 1;
        assert!(polls <= 400, "the stream never concluded");
        let reply = mcp
            .call_tool(
                id,
                "compute.stream_poll",
                json!({ "job_id": job_id, "since": since }),
            )
            .await;
        id += 1;
        let page: Value =
            serde_json::from_str(reply["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(page["job_id"].as_str().unwrap(), job_id);
        let next = page["next_seq"].as_u64().unwrap();
        assert!(next >= since, "the cursor never rewinds");
        for chunk in page["chunks"].as_array().unwrap() {
            assembled.push_str(chunk.as_str().unwrap());
        }
        since = next;
        match page["status"].as_str().unwrap() {
            "streaming" => tokio::time::sleep(Duration::from_millis(25)).await,
            "completed" => break reply,
            other => panic!("unexpected stream status {other}"),
        }
    };
    assert_eq!(
        assembled, "the streamed question",
        "the relayed feed assembles to the echoed input"
    );

    // The concluding poll carries the verified output and the receipt
    // metadata behind its bookkeeping block — the same blocks the
    // synchronous purchase answers with.
    let blocks = concluding["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 3, "bookkeeping, output, receipt metadata");
    assert_eq!(blocks[1]["text"], "the streamed question");
    let meta = last_block_json(&concluding);
    assert_eq!(meta["job_id"].as_str().unwrap(), job_id);
    assert_eq!(meta["receipt_verified"], true);
    assert_eq!(meta["stream_matched_output"], true);
    assert_eq!(meta["price_micro_usdc"], 25_000);

    // A concluded feed replays from any cursor.
    let replay = mcp
        .call_tool(
            id,
            "compute.stream_poll",
            json!({ "job_id": job_id, "since": 0 }),
        )
        .await;
    let page: Value = serde_json::from_str(replay["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(page["status"], "completed");
    let full: String = page["chunks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    assert_eq!(full, "the streamed question");

    // The purchase is on the books exactly once, spend counted.
    let receipts = last_block_json(&mcp.call_tool(id + 1, "compute.receipts", json!({})).await);
    assert_eq!(receipts["count"], 1);
    assert_eq!(receipts["receipts_failing_verification"], 0);
    assert_eq!(receipts["jobs"][0]["job_id"].as_str().unwrap(), job_id);

    let funds = last_block_json(&mcp.call_tool(id + 2, "compute.balance", json!({})).await);
    assert_eq!(funds["balance"]["charged_micro_usdc"], 25_000);
    assert_eq!(funds["balance"]["available_micro_usdc"], 75_000);

    mcp.shutdown().await;
}

#[tokio::test]
async fn the_stdio_binary_verifies_disputes_and_withdraws_through_its_own_tools() {
    let rig = Rig::launch().await;

    let home = tempfile::tempdir().unwrap();
    let buyer =
        LocalIdentity::load_or_create(&home.path().join("identity.json"), "buyer@compute").unwrap();
    rig.rail.preload(VerifiedDeposit {
        deposit_id: "money-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });
    // Withdrawing to the buyer's own wallet — any base58 address works,
    // this one is simply real.
    let recipient = buyer.agent_id().pubkey_base58();

    let mut mcp = Mcp::spawn(&rig.url, home.path()).await;
    initialize(&mut mcp).await;
    mcp.call_tool(3, "compute.deposit", json!({"deposit_id": "money-deposit"}))
        .await;
    let bought = last_block_json(
        &mcp.call_tool(
            4,
            "compute.infer",
            json!({ "prompt": "the audited question", "price_micro_usdc": 25_000 }),
        )
        .await,
    );
    let job_id = bought["job_id"].as_str().expect("job id").to_string();

    // The verify tool walks the whole trail this rig can offer: the
    // receipt re-verifies locally and the payout block is on the
    // books, but MockPayout submits no transaction — the honest
    // verdict is the off-chain record, not a failure.
    let verified = last_block_json(
        &mcp.call_tool(5, "compute.verify", json!({"job_id": job_id}))
            .await,
    );
    assert_eq!(verified["verdict"], "offchain_record_only");
    assert!(
        verified["memo"].as_str().unwrap().contains(&job_id),
        "the memo names the job it pays for"
    );
    assert_eq!(verified["tx_signature"], Value::Null);

    // A job outside this buyer's history refuses instead of guessing.
    let unknown = uuid::Uuid::new_v4();
    let refusal = mcp
        .refused(6, "compute.verify", json!({"job_id": unknown}))
        .await;
    assert!(
        refusal.contains(&unknown.to_string()) && refusal.contains("not in this buyer's history"),
        "the refusal names the unknown job: {refusal}"
    );

    // One dispute lands; its replay bounces at the coordinator and
    // rides back through the tool's error surface.
    let disputed = last_block_json(
        &mcp.call_tool(
            7,
            "compute.dispute",
            json!({"job_id": job_id, "reason": "the output was not the work"}),
        )
        .await,
    );
    assert_eq!(disputed["disputed"], true);
    assert_eq!(disputed["job_id"].as_str().unwrap(), job_id);
    assert!(!disputed["operator_pubkey_b58"].as_str().unwrap().is_empty());
    let refusal = mcp
        .refused(
            8,
            "compute.dispute",
            json!({"job_id": job_id, "reason": "again"}),
        )
        .await;
    assert!(refusal.starts_with("dispute failed:"), "{refusal}");

    // Withdraw without a client id: the server mints one, the mock
    // backend pushes instantly, and the memo re-derives from what the
    // view itself carries.
    let withdrawal = last_block_json(
        &mcp.call_tool(
            9,
            "compute.withdraw",
            json!({"amount_micro_usdc": 30_000, "recipient_address_b58": recipient}),
        )
        .await,
    );
    assert_eq!(withdrawal["pushed"], true);
    assert_eq!(withdrawal["amount_micro_usdc"], 30_000);
    assert_eq!(withdrawal["recipient_address_b58"], recipient);
    let withdrawal_id = withdrawal["withdrawal_id"].as_str().expect("minted id");
    uuid::Uuid::parse_str(withdrawal_id).expect("the minted id is a uuid");
    assert!(
        withdrawal["memo"].as_str().unwrap().contains(withdrawal_id),
        "the transfer memo names the withdrawal"
    );

    // Retrying the same withdrawal id is an echo, not a second debit.
    let replayed = last_block_json(
        &mcp.call_tool(
            10,
            "compute.withdraw",
            json!({
                "amount_micro_usdc": 30_000,
                "recipient_address_b58": recipient,
                "withdrawal_id": withdrawal_id,
            }),
        )
        .await,
    );
    assert_eq!(replayed["withdrawal_id"].as_str().unwrap(), withdrawal_id);

    // An overdraw refuses naming both numbers; nothing moves.
    let refusal = mcp
        .refused(
            11,
            "compute.withdraw",
            json!({"amount_micro_usdc": 1_000_000, "recipient_address_b58": recipient}),
        )
        .await;
    assert!(
        refusal.contains("1000000 micro-USDC requested") && refusal.contains("45000 available"),
        "the refusal names both numbers: {refusal}"
    );

    // The books close: one purchase, one withdrawal, nothing counted
    // twice by the replay or the refusals.
    let funds = last_block_json(&mcp.call_tool(12, "compute.balance", json!({})).await);
    assert_eq!(funds["balance"]["deposited_micro_usdc"], 100_000);
    assert_eq!(funds["balance"]["charged_micro_usdc"], 25_000);
    assert_eq!(funds["balance"]["withdrawn_micro_usdc"], 30_000);
    assert_eq!(funds["balance"]["available_micro_usdc"], 45_000);

    mcp.shutdown().await;
}

#[tokio::test]
async fn the_stdio_binary_binds_its_spend_caps_from_the_environment() {
    let rig = Rig::launch().await;

    let home = tempfile::tempdir().unwrap();
    let buyer =
        LocalIdentity::load_or_create(&home.path().join("identity.json"), "buyer@compute").unwrap();
    rig.rail.preload(VerifiedDeposit {
        deposit_id: "caps-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 50_000,
    });

    let mut mcp = Mcp::spawn_with(
        &rig.url,
        home.path(),
        &[
            ("COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC", "20000"),
            ("COVENANT_COMPUTE_MAX_TOTAL_MICRO_USDC", "30000"),
        ],
    )
    .await;
    initialize(&mut mcp).await;

    // The advertised surface carries the env ceiling, not the default.
    let listed = mcp
        .send(json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }))
        .await
        .unwrap();
    let infer = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "compute.infer")
        .expect("compute.infer is advertised")
        .clone();
    assert!(
        infer["description"]
            .as_str()
            .unwrap()
            .contains("capped at 20000 micro-USDC"),
        "the spec advertises the env ceiling"
    );

    mcp.call_tool(4, "compute.deposit", json!({"deposit_id": "caps-deposit"}))
        .await;

    // Above the per-call ceiling: refused before any dispatch.
    let refusal = mcp
        .refused(
            5,
            "compute.infer",
            json!({ "prompt": "too dear", "price_micro_usdc": 25_000 }),
        )
        .await;
    assert_eq!(
        refusal,
        "offered price 25000 micro-USDC exceeds the per-call ceiling 20000"
    );

    // A buy at the ceiling — an explicit price, to fix the spend the
    // session cap below is measured against. (The omitted-price default
    // is the market rate now, covered on its own elsewhere.)
    let bought = last_block_json(
        &mcp.call_tool(
            6,
            "compute.infer",
            json!({ "prompt": "first buy", "price_micro_usdc": 20_000 }),
        )
        .await,
    );
    assert_eq!(bought["price_micro_usdc"], 20_000);

    // The session cap counts that spend: an offer that would cross it
    // refuses up front, naming every component.
    let refusal = mcp
        .refused(
            7,
            "compute.infer",
            json!({ "prompt": "second buy", "price_micro_usdc": 15_000 }),
        )
        .await;
    assert_eq!(
        refusal,
        "session spend cap reached: 20000 spent + 0 in flight + 15000 offered \
         exceeds 30000 micro-USDC"
    );

    // Exactly-full headroom still buys — the cap is a ceiling, not a
    // strict bound.
    let bought = last_block_json(
        &mcp.call_tool(
            8,
            "compute.infer",
            json!({ "prompt": "second buy", "price_micro_usdc": 10_000 }),
        )
        .await,
    );
    assert_eq!(bought["price_micro_usdc"], 10_000);

    let funds = last_block_json(&mcp.call_tool(9, "compute.balance", json!({})).await);
    assert_eq!(funds["balance"]["charged_micro_usdc"], 30_000);

    mcp.shutdown().await;
}

/// The packaged binary's first-contact contract: `--help` and
/// `--version` answer and exit even with no coordinator configured —
/// a user asking a question gets an answer, never a server silently
/// waiting on stdin — and any unexpected argument refuses so a
/// misconfigured MCP client fails loud instead of hanging.
#[tokio::test]
async fn help_version_and_unexpected_arguments_answer_instead_of_serving() {
    let bare = |arg: &str| {
        Command::new(env!("CARGO_BIN_EXE_covenant-compute-mcp"))
            .env_clear()
            .arg(arg)
            .stdin(Stdio::null())
            .output()
    };

    let help = bare("--help").await.unwrap();
    assert!(
        help.status.success(),
        "--help exits 0 with no configuration at all"
    );
    let text = String::from_utf8_lossy(&help.stdout);
    for token in [
        "COVENANT_COMPUTE_COORDINATOR_URL",
        "COVENANT_COMPUTE_MAX_TOTAL_MICRO_USDC",
        "stdio",
    ] {
        assert!(text.contains(token), "usage names {token}: {text}");
    }
    // The total cap is a per-run counter that resets on restart, not a
    // durable lifetime budget; the help must say so, so a buyer never
    // over-trusts it as a hard cross-restart spend limit.
    assert!(
        text.contains("resets when the server restarts"),
        "usage states the total cap resets on restart: {text}"
    );
    assert!(
        !text.contains("lifetime spend"),
        "usage must not claim the total cap is a lifetime budget: {text}"
    );

    let version = bare("--version").await.unwrap();
    assert!(version.status.success(), "--version exits 0");
    assert_eq!(
        String::from_utf8_lossy(&version.stdout),
        format!("covenant-compute-mcp {}\n", env!("CARGO_PKG_VERSION")),
    );

    let unexpected = bare("--serve").await.unwrap();
    assert!(
        !unexpected.status.success(),
        "an unexpected argument refuses instead of speaking MCP"
    );
    let err = String::from_utf8_lossy(&unexpected.stderr);
    assert!(
        err.contains("unexpected argument") && err.contains("--help"),
        "{err}"
    );
}

/// The crash-window escape hatch, end to end through the spawned
/// binary: a keyed purchase dies with the process while its job sits
/// unaccepted, wedging the key and the money. A fresh server over the
/// same home cancels the job it finds in its own purchase book — the
/// refund lands, the key frees durably, and a retry of the cancel
/// answers the same fact.
#[tokio::test]
async fn a_crashed_keyed_purchase_is_cancelled_and_its_key_freed_on_restart() {
    // A market with capacity but no service: the one operator
    // registers (cheapest ask, wins the match) and never polls, so a
    // submitted job stays Offered until someone concludes it.
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let serve_state = state.clone();
    tokio::spawn(async move {
        axum::serve(listener, compute_router(serve_state))
            .await
            .unwrap();
    });
    let idle_operator = LocalIdentity::generate("idle-operator@test");
    let idle_key = idle_operator.agent_id().pubkey_base58();
    let profile = CapabilityProfile {
        operator: idle_operator.agent_id(),
        hardware: HardwareClass::CpuOnly,
        vram_gb: 0,
        models_served: vec!["any".into()],
        job_kinds: vec![JobKind::InferenceCall],
        price: PriceAsk {
            unit: PriceUnit::PerJob,
            micro_usdc: 500,
        },
        tee_capable: false,
        kind_prices: Vec::new(),
        kind_models: Vec::new(),
    };
    HttpCoordinatorClient::with_config(url.clone(), Duration::from_secs(5), 2)
        .register(RegisterRequest::sign(profile, payout_addr(8), &idle_operator).unwrap())
        .await
        .unwrap();

    let home = tempfile::tempdir().unwrap();
    let buyer =
        LocalIdentity::load_or_create(&home.path().join("identity.json"), "buyer@compute").unwrap();
    let buyer_key = buyer.agent_id().pubkey_base58();
    rail.preload(VerifiedDeposit {
        deposit_id: "crash-deposit".into(),
        buyer_pubkey_b58: buyer_key.clone(),
        amount_micro_usdc: 50_000,
    });

    // Life 1: fund, then die mid-purchase. The infer call will not
    // answer — its job has no server behind it — so it goes down the
    // pipe fire-and-forget and the process is killed once the job is
    // on the coordinator's books.
    let mut mcp = Mcp::spawn(&url, home.path()).await;
    initialize(&mut mcp).await;
    let claimed = last_block_json(
        &mcp.call_tool(3, "compute.deposit", json!({"deposit_id": "crash-deposit"}))
            .await,
    );
    assert_eq!(claimed["credited"], true);

    let doomed_buy = json!({
        "jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": { "name": "compute.infer", "arguments": {
            "prompt": "a purchase the process will not survive",
            "price_micro_usdc": 2_000,
            "deadline_ms": 120_000,
            "idempotency_key": "crash-key",
        }},
    });
    let mut line = doomed_buy.to_string();
    line.push('\n');
    mcp.stdin.write_all(line.as_bytes()).await.unwrap();
    mcp.stdin.flush().await.unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let job_id = loop {
        if let Some((job_id, record)) = state.jobs().by_buyer(&buyer_key).into_iter().next() {
            assert_eq!(
                record.phase,
                covenant_compute_coordinator::JobPhase::Offered
            );
            break job_id;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the doomed purchase reaches the coordinator"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    mcp.child.kill().await.unwrap();

    // Life 2 over the same home: the job id is sitting in the buyer's
    // own purchase book — cancel it.
    let mut mcp = Mcp::spawn(&url, home.path()).await;
    initialize(&mut mcp).await;
    let cancelled = last_block_json(
        &mcp.call_tool(5, "compute.cancel", json!({"job_id": job_id}))
            .await,
    );
    assert_eq!(cancelled["job_id"].as_str().unwrap(), job_id.to_string());
    assert_eq!(cancelled["status"], "refunded");
    assert_eq!(cancelled["refunded_micro_usdc"], 2_000);
    assert_eq!(
        cancelled["purchase_key_freed"], true,
        "the crashed key frees with the refund"
    );

    // The money is back and the market agrees end to end.
    let funds = last_block_json(&mcp.call_tool(6, "compute.balance", json!({})).await);
    assert_eq!(funds["balance"]["available_micro_usdc"], 50_000);
    assert_eq!(
        state.jobs().get(job_id).unwrap().phase,
        covenant_compute_coordinator::JobPhase::Refunded
    );
    assert!(
        !state.registry().queue_holds(&idle_key, job_id),
        "the dead offer leaves the idle operator's queue"
    );

    // Retrying the cancel answers the fact; there is no key left to
    // free.
    let retried = last_block_json(
        &mcp.call_tool(7, "compute.cancel", json!({"job_id": job_id}))
            .await,
    );
    assert_eq!(retried["status"], "refunded");
    assert_eq!(retried["purchase_key_freed"], false);
    mcp.shutdown().await;

    // Forensic close: the purchase book on disk holds no live entry
    // under the crashed key — a fresh call under it may buy again.
    let book =
        covenant_compute_buyer::PurchaseBook::open(&home.path().join("purchases.jsonl")).unwrap();
    assert!(book.lookup(&format!("{buyer_key}:crash-key")).is_none());
}
