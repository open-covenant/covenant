//! The `covenant-compute` human CLI end to end: the real spawned binary
//! driven over its own argv/stdout against a live coordinator and a
//! serving node — the loop a person actually runs. The hermetic twin of
//! a manual terminal session. `covenant-compute-mcp` covers the agent's
//! JSON-RPC path; this covers the human's.

use std::process::{Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use async_trait::async_trait;
use covenant_compute_coordinator::{
    router as compute_router, CoordinatorConfig, CoordinatorState, MockPayout, MockRail,
    NoReputation, VerifiedDeposit,
};
use covenant_compute_node::{
    Coordinator as _, EchoExecutor, ExecutionOutcome, ExecutorError, HttpCoordinatorClient,
    InMemoryEarningsLedger, JobExecutor, LeaseControl, LeaseExecutor, Node, NodeConfig,
    StubSessionBackend,
};
use covenant_compute_protocol::{
    speech_output, CapabilityProfile, FundingSource, HardwareClass, JobEnvelopePayload, JobKind,
    PriceAsk, PriceUnit, RegisterRequest,
};
use covenant_identity::LocalIdentity;
use tokio::process::Command;

/// One live market: a coordinator with journaled books and a claimable
/// deposit rail, plus a registered node serving the echo executor.
struct Rig {
    url: String,
    rail: Arc<MockRail>,
    _coordinator_home: tempfile::TempDir,
}

impl Rig {
    async fn launch() -> Self {
        Self::launch_with(
            EchoExecutor,
            vec![JobKind::InferenceCall, JobKind::BatchJob],
            vec!["any".into()],
        )
        .await
    }

    /// A market whose node synthesizes speech, returning a fixed mock clip
    /// — the supply side the CLI's `speak` buys from, so the buy-and-write
    /// path can be driven end to end without a real synthesizer (proven
    /// against `say` in the node's own tests).
    async fn launch_speaking() -> Self {
        Self::launch_with(
            SpeakMock,
            vec![JobKind::SpeechSynthesis],
            vec!["say-1".into()],
        )
        .await
    }

    async fn launch_with<X: JobExecutor + 'static>(
        executor: X,
        job_kinds: Vec<JobKind>,
        models_served: Vec<String>,
    ) -> Self {
        Self::launch_building(|_| executor, job_kinds, models_served).await
    }

    /// A market whose node serves lease sessions from a stub backend: it
    /// publishes a fixed endpoint and holds the session open, so the CLI's
    /// open/view/close loop drives end to end without renting a real
    /// machine (the broker backend is proven against a mock market in the
    /// node's own suite). The executor watches the coordinator for the
    /// buyer's close, the same wire the real node uses.
    async fn launch_leasing() -> Self {
        Self::launch_building(
            |client| {
                LeaseExecutor::new(
                    Arc::new(StubSessionBackend::new("ssh renter@stub.test -p 2222")),
                    LeaseControl::new(),
                )
                .watching(client)
                .with_poll_interval(Duration::from_millis(100))
            },
            vec![JobKind::LeaseSession],
            vec!["any".into()],
        )
        .await
    }

    /// Like [`launch_with`], but the executor is built from the registered
    /// coordinator client — the shape a lease node needs, whose executor
    /// watches that same client for the buyer's close.
    async fn launch_building<X: JobExecutor + 'static>(
        build_executor: impl FnOnce(Arc<HttpCoordinatorClient>) -> X,
        job_kinds: Vec<JobKind>,
        models_served: Vec<String>,
    ) -> Self {
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
            models_served,
            job_kinds,
            price: PriceAsk {
                unit: PriceUnit::PerLeaseHour,
                micro_usdc: 10_000,
            },
            tee_capable: false,
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
        let executor = build_executor(client.clone());
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

/// The fixed clip a speech operator hands back in these tests: a small,
/// recognizable byte string, not a real WAV — the CLI's contract is to
/// carry the operator's exact bytes to the buyer's file, so a test asserts
/// the written file equals these bytes. Real synthesis is proven against
/// `say` in the node's own suite.
const MOCK_CLIP: &[u8] = b"RIFF\x00\x00\x00\x00WAVEmock-say-clip";

#[derive(Clone, Copy)]
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

/// Run the binary against a coordinator over a given home, argv only —
/// no stdin, the CLI never reads it. Returns the whole `Output` so a
/// test can hold it to both the exit status and the streams.
async fn run_cli(url: &str, home: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_covenant-compute"))
        .env_clear()
        .env("HOME", home) // guardrail: no real ~ is ever touched
        .env("COVENANT_COMPUTE_COORDINATOR_URL", url)
        .env("COVENANT_COMPUTE_MCP_HOME", home)
        .current_dir(home) // a `speak` clip is written beside the caller
        .args(args)
        .output()
        .await
        .expect("spawn covenant-compute")
}

/// Run the binary with `input` piped to its stdin — the `-` reads-from-
/// stdin path a long or multi-line prompt takes.
async fn run_cli_stdin(url: &str, home: &std::path::Path, args: &[&str], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_covenant-compute"))
        .env_clear()
        .env("HOME", home)
        .env("COVENANT_COMPUTE_COORDINATOR_URL", url)
        .env("COVENANT_COMPUTE_MCP_HOME", home)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn covenant-compute");
    let mut stdin = child.stdin.take().expect("child stdin");
    stdin.write_all(input.as_bytes()).await.unwrap();
    drop(stdin); // EOF
    child.wait_with_output().await.expect("wait on child")
}

fn stdout_of(output: &Output) -> String {
    assert!(
        output.status.success(),
        "command failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The command was expected to refuse — return its stderr for the
/// caller to hold to the reason.
fn stderr_of_failure(output: &Output) -> String {
    assert!(
        !output.status.success(),
        "command was expected to fail but succeeded: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[tokio::test]
async fn the_cli_funds_a_wallet_and_reads_the_market_end_to_end() {
    let rig = Rig::launch().await;
    let home = tempfile::tempdir().unwrap();

    // The identity file is created before the deposit can name it; the
    // binary loads the same file every run.
    let buyer =
        LocalIdentity::load_or_create(&home.path().join("identity.json"), "buyer@compute").unwrap();
    let buyer_key = buyer.agent_id().pubkey_base58();
    rig.rail.preload(VerifiedDeposit {
        deposit_id: "cli-deposit".into(),
        buyer_pubkey_b58: buyer_key.clone(),
        amount_micro_usdc: 100_000,
    });

    // whoami answers with no coordinator at all — a purely local read.
    let bare = Command::new(env!("CARGO_BIN_EXE_covenant-compute"))
        .env_clear()
        .env("HOME", home.path())
        .env("COVENANT_COMPUTE_MCP_HOME", home.path())
        .arg("whoami")
        .output()
        .await
        .unwrap();
    let who = stdout_of(&bare);
    assert!(who.contains(&buyer_key), "whoami names this buyer: {who}");
    assert!(
        who.contains("(unset"),
        "whoami flags the missing coordinator: {who}"
    );

    // whoami --json is the same facts as a parseable record, for a script.
    let bare_json = Command::new(env!("CARGO_BIN_EXE_covenant-compute"))
        .env_clear()
        .env("HOME", home.path())
        .env("COVENANT_COMPUTE_MCP_HOME", home.path())
        .args(["whoami", "--json"])
        .output()
        .await
        .unwrap();
    let who_json: serde_json::Value =
        serde_json::from_str(&stdout_of(&bare_json)).expect("whoami --json is a JSON record");
    assert_eq!(who_json["pubkey"], serde_json::json!(buyer_key));
    assert!(
        who_json["coordinator"].is_null(),
        "an unset coordinator is null: {who_json}"
    );

    // The market shows the rig's one node as a purchasable row.
    let market = stdout_of(&run_cli(&rig.url, home.path(), &["capacity"]).await);
    assert!(
        market.contains("1 matchable now"),
        "one node matchable: {market}"
    );
    assert!(
        market.contains("inference_call"),
        "the row names its kind: {market}"
    );
    assert!(
        market.contains("10000"),
        "the row names the ask floor: {market}"
    );

    // The same read as JSON for scripting.
    let market_json = stdout_of(&run_cli(&rig.url, home.path(), &["capacity", "--json"]).await);
    let parsed: serde_json::Value = serde_json::from_str(&market_json).expect("valid json");
    assert_eq!(parsed["matchable_operators"], 1);

    // Claim the deposit, then see it on the balance.
    let claimed = stdout_of(&run_cli(&rig.url, home.path(), &["deposit", "cli-deposit"]).await);
    assert!(claimed.contains("credited"), "the claim credits: {claimed}");
    assert!(
        claimed.contains("100000"),
        "the claim names the amount: {claimed}"
    );

    let balance = stdout_of(&run_cli(&rig.url, home.path(), &["balance"]).await);
    assert!(
        balance.contains(&buyer_key),
        "the balance names the buyer: {balance}"
    );
    assert!(
        balance.contains("deposited:") && balance.contains("100000"),
        "the balance shows the deposit: {balance}"
    );
    assert!(
        balance.contains("available:") && balance.contains("100000"),
        "nothing spent yet, all available: {balance}"
    );

    // Re-claiming the same deposit is an honest no-op, not an error.
    let reclaim = stdout_of(&run_cli(&rig.url, home.path(), &["deposit", "cli-deposit"]).await);
    assert!(
        reclaim.contains("already claimed"),
        "the re-claim reports the no-op: {reclaim}"
    );
}

#[tokio::test]
async fn a_coordinator_url_without_a_scheme_is_refused_with_a_clear_message() {
    let home = tempfile::tempdir().unwrap();
    // No http:// — reqwest would otherwise die deep in URL parsing; the
    // CLI names the fix up front instead.
    let refusal = stderr_of_failure(&run_cli("localhost:8080", home.path(), &["balance"]).await);
    assert!(
        refusal.contains("must start with http:// or https://"),
        "the refusal names the fix: {refusal}"
    );
}

#[tokio::test]
async fn a_default_buy_offers_the_market_rate_not_the_ceiling() {
    let rig = Rig::launch().await;
    let home = tempfile::tempdir().unwrap();
    let buyer =
        LocalIdentity::load_or_create(&home.path().join("identity.json"), "buyer@compute").unwrap();
    rig.rail.preload(VerifiedDeposit {
        deposit_id: "market-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });
    run_cli(&rig.url, home.path(), &["deposit", "market-deposit"]).await;

    // No --price: the CLI offers the cheapest matching ask (the rig node
    // asks 10_000), not the per-call ceiling ($1). Settlement charges the
    // envelope price, so a default buy must not hand the operator a
    // ceiling it never asked for.
    let out = run_cli(&rig.url, home.path(), &["infer", "priced at market"]).await;
    let bought = stdout_of(&out);
    assert!(
        bought.contains("price:") && bought.contains("10000"),
        "the default offer is the 10000 ask, not the ceiling: {bought}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("offering 10000 micro-USDC"),
        "the CLI says what it offered: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The books charged the market rate, not the ceiling.
    let balance = stdout_of(&run_cli(&rig.url, home.path(), &["balance"]).await);
    assert!(
        balance.contains("charged:") && balance.contains("10000"),
        "only the 10000 market rate was charged: {balance}"
    );
}

#[tokio::test]
async fn the_cli_buys_verifies_and_lists_receipts_end_to_end() {
    let rig = Rig::launch().await;
    let home = tempfile::tempdir().unwrap();
    let buyer =
        LocalIdentity::load_or_create(&home.path().join("identity.json"), "buyer@compute").unwrap();
    rig.rail.preload(VerifiedDeposit {
        deposit_id: "buy-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });
    run_cli(&rig.url, home.path(), &["deposit", "buy-deposit"]).await;

    // Buy an inference call: the echoed output rides on stdout, the
    // locally re-verified receipt underneath it.
    let bought = stdout_of(
        &run_cli(
            &rig.url,
            home.path(),
            &["infer", "the terminal question", "--price", "25000"],
        )
        .await,
    );
    assert!(
        bought.contains("the terminal question"),
        "the output leads: {bought}"
    );
    assert!(
        bought.contains("verified:  yes"),
        "the receipt verified locally: {bought}"
    );
    assert!(bought.contains("price:") && bought.contains("25000"));

    // A price above the per-call ceiling refuses before any dispatch.
    let refusal = stderr_of_failure(
        &run_cli(
            &rig.url,
            home.path(),
            &["infer", "too dear", "--price", "2000000"],
        )
        .await,
    );
    assert!(
        refusal.contains("exceeds the per-call ceiling"),
        "the ceiling refusal names itself: {refusal}"
    );

    // Buy a batch command too; the echo node returns the command text.
    let ran = stdout_of(
        &run_cli(
            &rig.url,
            home.path(),
            &["run", "echo hello from the batch", "--price", "25000"],
        )
        .await,
    );
    assert!(
        ran.contains("echo hello from the batch"),
        "the batch output echoes the command: {ran}"
    );

    // Receipts list both purchases, both verified; grab a job id from
    // the JSON view for the verify below.
    let receipts_json = stdout_of(&run_cli(&rig.url, home.path(), &["receipts", "--json"]).await);
    let rows: serde_json::Value = serde_json::from_str(&receipts_json).unwrap();
    let rows = rows.as_array().expect("receipts is an array");
    assert_eq!(rows.len(), 2, "both purchases are on the books: {rows:?}");
    assert!(
        rows.iter().all(|r| r["receipt_verified"] == true),
        "every receipt verified: {rows:?}"
    );
    let job_id = rows[0]["job_id"].as_str().expect("a job id").to_string();

    let receipts_human = stdout_of(&run_cli(&rig.url, home.path(), &["receipts"]).await);
    assert!(receipts_human.contains("2 job(s), 0 failing"));
    assert!(receipts_human.contains("verified"));

    // Re-read a past job's output straight from the coordinator, proven
    // locally — the answer the buyer paid for survives a lost terminal,
    // which `receipts` (hash only) cannot give back.
    let reread = stdout_of(&run_cli(&rig.url, home.path(), &["output", &job_id]).await);
    assert!(
        reread.contains("verified:  yes"),
        "the re-read receipt verifies locally: {reread}"
    );
    assert!(
        reread.contains("the terminal question") || reread.contains("echo hello from the batch"),
        "the paid-for output is re-printed, not just its hash: {reread}"
    );

    // A job outside this buyer's history can't be read back.
    let unknown_out = stderr_of_failure(
        &run_cli(
            &rig.url,
            home.path(),
            &["output", &uuid::Uuid::new_v4().to_string()],
        )
        .await,
    );
    assert!(
        unknown_out.contains("no such job") || unknown_out.contains("404"),
        "an unknown job's output refuses by name: {unknown_out}"
    );

    // Verify walks the money trail as far as this rig offers: the
    // receipt re-verifies and the payout is on the books, but MockPayout
    // submits no transaction — the honest verdict is the off-chain
    // record, not a failure.
    let verified = stdout_of(&run_cli(&rig.url, home.path(), &["verify", &job_id]).await);
    assert!(
        verified.contains("verdict:  offchain_record_only"),
        "the honest off-chain verdict: {verified}"
    );
    assert!(
        verified.contains(&job_id),
        "the verify names the job: {verified}"
    );

    // A job outside this buyer's history refuses instead of guessing.
    let unknown = uuid::Uuid::new_v4();
    let refusal =
        stderr_of_failure(&run_cli(&rig.url, home.path(), &["verify", &unknown.to_string()]).await);
    assert!(
        refusal.contains("not in this buyer's history"),
        "the unknown job refuses by name: {refusal}"
    );

    // The books close: two purchases charged, the rest available.
    let balance = stdout_of(&run_cli(&rig.url, home.path(), &["balance"]).await);
    assert!(
        balance.contains("charged:") && balance.contains("50000"),
        "two 25000 buys charged: {balance}"
    );
}

#[tokio::test]
async fn infer_stream_prints_tokens_and_the_verified_receipt() {
    let rig = Rig::launch().await;
    let home = tempfile::tempdir().unwrap();
    let buyer =
        LocalIdentity::load_or_create(&home.path().join("identity.json"), "buyer@compute").unwrap();
    rig.rail.preload(VerifiedDeposit {
        deposit_id: "stream-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });
    run_cli(&rig.url, home.path(), &["deposit", "stream-deposit"]).await;

    // The echo node streams its input back, so `--stream` shows the
    // prompt as it arrives with the verified receipt underneath. (A node
    // that couldn't stream would print the same output once at the end,
    // so this holds either way.)
    let streamed = stdout_of(
        &run_cli(
            &rig.url,
            home.path(),
            &[
                "infer",
                "stream these tokens",
                "--stream",
                "--price",
                "25000",
            ],
        )
        .await,
    );
    assert!(
        streamed.contains("stream these tokens"),
        "the streamed output lands on stdout: {streamed}"
    );
    assert!(
        streamed.contains("verified:  yes"),
        "the verified receipt follows the stream: {streamed}"
    );

    // `--stream` and `--json` contradict: one prints tokens live, the
    // other a single record. The refusal names the conflict, and nothing
    // is spent — the balance below is untouched by it.
    let refusal = stderr_of_failure(
        &run_cli(
            &rig.url,
            home.path(),
            &["infer", "no", "--stream", "--json", "--price", "25000"],
        )
        .await,
    );
    assert!(
        refusal.contains("can't combine with --json"),
        "the conflict refuses by name: {refusal}"
    );

    let balance = stdout_of(&run_cli(&rig.url, home.path(), &["balance"]).await);
    assert!(
        balance.contains("charged:") && balance.contains("25000"),
        "only the one streamed buy charged; the refused combo spent nothing: {balance}"
    );
}

#[tokio::test]
async fn infer_reads_a_piped_prompt_from_stdin() {
    let rig = Rig::launch().await;
    let home = tempfile::tempdir().unwrap();
    let buyer =
        LocalIdentity::load_or_create(&home.path().join("identity.json"), "buyer@compute").unwrap();
    rig.rail.preload(VerifiedDeposit {
        deposit_id: "stdin-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });
    run_cli(&rig.url, home.path(), &["deposit", "stdin-deposit"]).await;

    // A multi-line prompt no one would pass on the argv, piped in behind
    // the `-`, echoes back through the whole real loop.
    let prompt = "a multi-line prompt\nno one would type as an argument";
    let bought = stdout_of(
        &run_cli_stdin(
            &rig.url,
            home.path(),
            &["infer", "-", "--price", "25000"],
            prompt,
        )
        .await,
    );
    assert!(
        bought.contains("a multi-line prompt") && bought.contains("no one would type"),
        "the whole piped prompt survived: {bought}"
    );
    assert!(bought.contains("verified:  yes"));

    // Empty stdin refuses rather than buying nothing.
    let empty =
        stderr_of_failure(&run_cli_stdin(&rig.url, home.path(), &["infer", "-"], "   ").await);
    assert!(
        empty.contains("stdin was empty"),
        "empty stdin refuses: {empty}"
    );
}

#[tokio::test]
async fn the_cli_disputes_withdraws_and_refuses_an_uncancellable_job() {
    let rig = Rig::launch().await;
    let home = tempfile::tempdir().unwrap();
    let buyer =
        LocalIdentity::load_or_create(&home.path().join("identity.json"), "buyer@compute").unwrap();
    let recipient = buyer.agent_id().pubkey_base58();
    rig.rail.preload(VerifiedDeposit {
        deposit_id: "money-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });
    run_cli(&rig.url, home.path(), &["deposit", "money-deposit"]).await;
    run_cli(
        &rig.url,
        home.path(),
        &["infer", "the audited question", "--price", "25000"],
    )
    .await;

    let receipts: serde_json::Value = serde_json::from_str(&stdout_of(
        &run_cli(&rig.url, home.path(), &["receipts", "--json"]).await,
    ))
    .unwrap();
    let job_id = receipts[0]["job_id"].as_str().unwrap().to_string();

    // Dispute the completed job; a second dispute of the same job bounces
    // at the coordinator and rides back through the CLI's error surface.
    let disputed =
        stdout_of(&run_cli(&rig.url, home.path(), &["dispute", &job_id, "wrong output"]).await);
    assert!(
        disputed.contains("dispute recorded") && disputed.contains(&job_id),
        "the dispute lands and names the job: {disputed}"
    );
    let again =
        stderr_of_failure(&run_cli(&rig.url, home.path(), &["dispute", &job_id, "again"]).await);
    assert!(again.contains("dispute"), "the replay refuses: {again}");

    // A completed job cannot be cancelled — committed work settles by
    // result, not a buyer's later change of mind.
    let uncancellable =
        stderr_of_failure(&run_cli(&rig.url, home.path(), &["cancel", &job_id]).await);
    assert!(
        uncancellable.contains("cancel"),
        "the refusal comes from the cancel path: {uncancellable}"
    );

    // Withdraw unspent balance; the mock backend pushes instantly and the
    // memo names the withdrawal. Reusing the id is an echo, not a second
    // debit.
    let withdrawal: serde_json::Value = serde_json::from_str(&stdout_of(
        &run_cli(
            &rig.url,
            home.path(),
            &["withdraw", "30000", &recipient, "--json"],
        )
        .await,
    ))
    .unwrap();
    assert_eq!(withdrawal["pushed"], true);
    assert_eq!(withdrawal["amount_micro_usdc"], 30000);
    let withdrawal_id = withdrawal["withdrawal_id"].as_str().unwrap();
    assert!(withdrawal["memo"].as_str().unwrap().contains(withdrawal_id));

    let replay = stdout_of(
        &run_cli(
            &rig.url,
            home.path(),
            &[
                "withdraw",
                "30000",
                &recipient,
                "--withdrawal-id",
                withdrawal_id,
            ],
        )
        .await,
    );
    assert!(
        replay.contains(withdrawal_id),
        "the reused id echoes the same withdrawal: {replay}"
    );

    // An overdraw refuses, naming the shortfall; nothing moves.
    let overdraw = stderr_of_failure(
        &run_cli(&rig.url, home.path(), &["withdraw", "1000000", &recipient]).await,
    );
    assert!(
        overdraw.contains("available"),
        "the overdraw names the available balance: {overdraw}"
    );

    let history = stdout_of(&run_cli(&rig.url, home.path(), &["withdrawals"]).await);
    assert!(
        history.contains(withdrawal_id) && history.contains("pushed"),
        "the one withdrawal is on the history: {history}"
    );

    // The books close: one buy charged, one withdrawal, the rest
    // available — nothing double-counted by the replay or the refusals.
    let balance = stdout_of(&run_cli(&rig.url, home.path(), &["balance"]).await);
    assert!(balance.contains("charged:") && balance.contains("25000"));
    assert!(balance.contains("withdrawn:") && balance.contains("30000"));
    assert!(
        balance.contains("available:") && balance.contains("45000"),
        "100000 - 25000 - 30000 = 45000 available: {balance}"
    );
}

/// The packaged binary's first-contact contract: `--help`, `--version`,
/// a bare invocation, `--json` with no command, and `<command> --help`
/// all answer and exit 0 with no configuration at all, and an unknown
/// command refuses loudly instead of half-running.
#[tokio::test]
async fn help_version_and_unknown_commands_answer_without_a_coordinator() {
    let bare = |args: &'static [&'static str]| {
        Command::new(env!("CARGO_BIN_EXE_covenant-compute"))
            .env_clear()
            .args(args)
            .output()
    };

    // `--json` alone once panicked (the global flag was stripped, then the
    // command popped off an empty vec) — it must answer like a bare call.
    // `<command> --help` must answer with usage too, never read the flag
    // as an argument (`withdraw --help` once became a bad amount) or run
    // the command (`infer --help` once tried the buy and died on the
    // missing coordinator this env_clear() withholds).
    for args in [
        &["--help"][..],
        &["-h"][..],
        &[][..],
        &["--json"][..],
        &["infer", "--help"][..],
        &["embed", "--help"][..],
        &["run", "-h"][..],
        &["deposit", "--help"][..],
        &["withdraw", "--help"][..],
        &["verify", "-h"][..],
    ] {
        let out = bare(args).await.unwrap();
        assert!(out.status.success(), "{args:?} exits 0 with no config");
        let text = String::from_utf8_lossy(&out.stdout);
        for token in [
            "Usage:",
            "COVENANT_COMPUTE_COORDINATOR_URL",
            "whoami",
            "embed",
        ] {
            assert!(text.contains(token), "usage names {token}: {text}");
        }
    }

    let version = bare(&["--version"]).await.unwrap();
    assert!(version.status.success(), "--version exits 0");
    assert_eq!(
        String::from_utf8_lossy(&version.stdout),
        format!("covenant-compute {}\n", env!("CARGO_PKG_VERSION")),
    );

    let unknown = bare(&["frobnicate"]).await.unwrap();
    assert!(!unknown.status.success(), "an unknown command refuses");
    let err = String::from_utf8_lossy(&unknown.stderr);
    assert!(
        err.contains("unknown command") && err.contains("--help"),
        "{err}"
    );
}

#[tokio::test]
async fn embed_refuses_when_no_operator_serves_embeddings() {
    // The rig's only node serves inference and batch, not embeddings, so
    // an embedding buy has no market. The CLI must refuse at capacity
    // lookup — before signing or dispatching anything — and name the
    // capability nobody serves.
    let rig = Rig::launch().await;
    let home = tempfile::tempdir().unwrap();
    let refusal =
        stderr_of_failure(&run_cli(&rig.url, home.path(), &["embed", "vectorize this"]).await);
    assert!(
        refusal.contains("no operator is serving")
            && refusal.contains("embedding")
            && refusal.contains("nothing was dispatched"),
        "the refusal names the unserved embedding capability: {refusal}"
    );
}

#[tokio::test]
async fn transcribe_refuses_when_no_operator_serves_transcription() {
    // The rig's only node serves inference and batch, not transcription,
    // so a transcription buy has no market. The CLI must read --audio,
    // build the job, then refuse at capacity lookup — before signing or
    // dispatching anything — and name the capability nobody serves.
    let rig = Rig::launch().await;
    let home = tempfile::tempdir().unwrap();
    let clip = home.path().join("clip.wav");
    std::fs::write(&clip, b"not-real-audio-bytes").unwrap();
    let refusal = stderr_of_failure(
        &run_cli(
            &rig.url,
            home.path(),
            &["transcribe", "--audio", clip.to_str().unwrap()],
        )
        .await,
    );
    assert!(
        refusal.contains("no operator is serving")
            && refusal.contains("transcription")
            && refusal.contains("nothing was dispatched"),
        "the refusal names the unserved transcription capability: {refusal}"
    );
}

#[tokio::test]
async fn speak_refuses_when_no_operator_serves_speech() {
    // The rig's only node serves inference and batch, not speech, so a
    // text-to-speech buy has no market. The CLI must read the text, build
    // the job, then refuse at capacity lookup — before signing, paying, or
    // writing any clip — and name the capability nobody serves.
    let rig = Rig::launch().await;
    let home = tempfile::tempdir().unwrap();
    let refusal =
        stderr_of_failure(&run_cli(&rig.url, home.path(), &["speak", "say something"]).await);
    assert!(
        refusal.contains("no operator is serving")
            && refusal.contains("speech_synthesis")
            && refusal.contains("nothing was dispatched"),
        "the refusal names the unserved speech capability: {refusal}"
    );
}

#[tokio::test]
async fn speak_buys_a_clip_and_writes_it_to_a_file() {
    let rig = Rig::launch_speaking().await;
    let home = tempfile::tempdir().unwrap();
    let buyer =
        LocalIdentity::load_or_create(&home.path().join("identity.json"), "buyer@compute").unwrap();
    rig.rail.preload(VerifiedDeposit {
        deposit_id: "speak-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });
    run_cli(&rig.url, home.path(), &["deposit", "speak-deposit"]).await;

    // Buy a clip: the CLI dispatches, verifies the receipt, and writes the
    // operator's audio to a file beside the working directory.
    let out = stdout_of(
        &run_cli(
            &rig.url,
            home.path(),
            &["speak", "hello from covenant", "--price", "25000"],
        )
        .await,
    );
    assert!(
        out.contains("wrote") && out.contains("audio to"),
        "the CLI reports the written clip: {out}"
    );
    assert!(
        out.contains("verified:  yes"),
        "the receipt verified locally under the clip: {out}"
    );

    // The clip is on disk and holds exactly the bytes the operator sent —
    // the CLI carried the audio through, it did not re-encode it.
    let clip = std::fs::read_dir(home.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("speech-") && n.ends_with(".wav"))
        })
        .expect("a speech-<job>.wav clip was written beside the caller");
    assert_eq!(
        std::fs::read(&clip).unwrap(),
        MOCK_CLIP,
        "the written file holds the operator's exact audio bytes"
    );
}

/// The count of purchases on the books, and the first one's job id.
fn receipts_count_and_first_job(json: &str) -> (usize, String) {
    let rows: serde_json::Value = serde_json::from_str(json).unwrap();
    let rows = rows.as_array().expect("receipts is an array");
    let first = rows
        .first()
        .map(|r| r["job_id"].as_str().expect("a job id").to_string())
        .unwrap_or_default();
    (rows.len(), first)
}

#[tokio::test]
async fn a_keyed_buy_replays_on_retry_instead_of_paying_twice() {
    // The money-safety property of --idempotency-key: a purchase made
    // under a key is exactly-once. A retry with the same key and
    // arguments replays the recorded answer, never minting a second job
    // or a second charge; a reused key with a different question refuses
    // rather than serve the old answer to a new one.
    let rig = Rig::launch().await;
    let home = tempfile::tempdir().unwrap();
    let buyer =
        LocalIdentity::load_or_create(&home.path().join("identity.json"), "buyer@compute").unwrap();
    rig.rail.preload(VerifiedDeposit {
        deposit_id: "keyed-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });
    run_cli(&rig.url, home.path(), &["deposit", "keyed-deposit"]).await;

    let key = "retry-me";
    let first = stdout_of(
        &run_cli(
            &rig.url,
            home.path(),
            &[
                "infer",
                "keyed question",
                "--price",
                "25000",
                "--idempotency-key",
                key,
            ],
        )
        .await,
    );
    assert!(
        first.contains("keyed question"),
        "the first buy answers: {first}"
    );
    assert!(
        first.contains("verified:  yes"),
        "the receipt verifies locally: {first}"
    );
    let (n1, job1) = receipts_count_and_first_job(&stdout_of(
        &run_cli(&rig.url, home.path(), &["receipts", "--json"]).await,
    ));
    assert_eq!(n1, 1, "one purchase on the books after the first buy");

    // Retry under the same key and arguments: replays, no second job.
    let retry = stdout_of(
        &run_cli(
            &rig.url,
            home.path(),
            &[
                "infer",
                "keyed question",
                "--price",
                "25000",
                "--idempotency-key",
                key,
            ],
        )
        .await,
    );
    assert!(
        retry.contains("keyed question"),
        "the retry replays the answer: {retry}"
    );
    let (n2, job2) = receipts_count_and_first_job(&stdout_of(
        &run_cli(&rig.url, home.path(), &["receipts", "--json"]).await,
    ));
    assert_eq!(n2, 1, "the retry did not mint a second job");
    assert_eq!(job2, job1, "the retry replayed the very same job");

    // The books close: charged once, never twice.
    let balance = stdout_of(&run_cli(&rig.url, home.path(), &["balance"]).await);
    assert!(
        balance.contains("charged:") && balance.contains("25000") && !balance.contains("50000"),
        "one 25000 buy charged, not two: {balance}"
    );

    // A reused key with a different question refuses, naming what changed.
    let conflict = stderr_of_failure(
        &run_cli(
            &rig.url,
            home.path(),
            &[
                "infer",
                "a different question",
                "--price",
                "25000",
                "--idempotency-key",
                key,
            ],
        )
        .await,
    );
    assert!(
        conflict.contains("already used with a different") && conflict.contains("input"),
        "the reused key refuses by naming the changed argument: {conflict}"
    );
}

#[tokio::test]
async fn a_keyed_infer_refuses_to_stream_and_bounds_the_key() {
    let rig = Rig::launch().await;
    let home = tempfile::tempdir().unwrap();

    // Streaming and idempotency don't mix: a stream hands back a job id
    // before the answer is verified, so a retry has nothing to replay.
    let streamed = stderr_of_failure(
        &run_cli(
            &rig.url,
            home.path(),
            &["infer", "q", "--stream", "--idempotency-key", "k"],
        )
        .await,
    );
    assert!(
        streamed.contains("idempotency-key") && streamed.contains("stream"),
        "stream + key is refused, and the reason names both: {streamed}"
    );

    // An empty key is refused with its bound, before the book is opened.
    let empty = stderr_of_failure(
        &run_cli(
            &rig.url,
            home.path(),
            &["infer", "q", "--idempotency-key", ""],
        )
        .await,
    );
    assert!(
        empty.contains("1..=128"),
        "an empty key is refused with the bound: {empty}"
    );
}

#[tokio::test]
async fn a_dry_run_previews_the_real_price_and_buys_nothing() {
    let rig = Rig::launch().await;
    let home = tempfile::tempdir().unwrap();

    // No deposit: a dry run resolves the market and prints the price a buy
    // would pay without touching the buyer's balance, so a broke buyer can
    // still preview. The rig node asks 10_000; the preview must show it.
    let preview =
        stdout_of(&run_cli(&rig.url, home.path(), &["infer", "--dry-run", "preview me"]).await);
    assert!(
        preview.contains("dry run") && preview.contains("nothing dispatched"),
        "the preview says it dispatched nothing: {preview}"
    );
    assert!(
        preview.contains("price") && preview.contains("10000"),
        "the preview shows the 10000 cheapest matching ask: {preview}"
    );
    assert!(
        preview.contains("cheapest matching ask"),
        "the preview names where the price came from: {preview}"
    );

    // The machine-readable form carries the same figure for a scripting
    // buyer, tagged as a dry run so nothing downstream mistakes it for a
    // receipt.
    let json = stdout_of(
        &run_cli(
            &rig.url,
            home.path(),
            &["infer", "--dry-run", "--json", "preview me"],
        )
        .await,
    );
    let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(doc["dry_run"], serde_json::json!(true));
    assert_eq!(doc["price_micro_usdc"], serde_json::json!(10_000));
    assert_eq!(
        doc["price_source"],
        serde_json::json!("cheapest_matching_ask")
    );
    assert_eq!(doc["kind"], serde_json::json!("inference_call"));

    // The same preview serves a batch buy: `run` is the other kind the rig
    // node offers, and it previews with no model to route by.
    let batch = stdout_of(&run_cli(&rig.url, home.path(), &["run", "--dry-run", "echo hi"]).await);
    assert!(
        batch.contains("batch_job") && batch.contains("10000"),
        "a batch run previews its kind and price: {batch}"
    );

    // The contract: no job was created and no funds moved. The buyer's
    // history is still empty.
    let receipts = stdout_of(&run_cli(&rig.url, home.path(), &["receipts"]).await);
    assert!(
        receipts.contains("no jobs yet"),
        "a dry run leaves the history empty: {receipts}"
    );

    // An explicit --price previews that figure and names it as the buyer's
    // own, distinct from a defaulted ask.
    let explicit = stdout_of(
        &run_cli(
            &rig.url,
            home.path(),
            &["infer", "--dry-run", "--price", "20000", "q"],
        )
        .await,
    );
    assert!(
        explicit.contains("20000") && explicit.contains("your --price"),
        "an explicit price previews as the buyer's own: {explicit}"
    );

    // --dry-run buys nothing, so it can't pair with the flags that only
    // make sense once a purchase is real: a stream to print, or a key to
    // reserve. Both refuse before any market touch.
    let with_stream = stderr_of_failure(
        &run_cli(
            &rig.url,
            home.path(),
            &["infer", "--dry-run", "--stream", "q"],
        )
        .await,
    );
    assert!(
        with_stream.contains("dry-run") && with_stream.contains("stream"),
        "dry-run + stream is refused, naming both: {with_stream}"
    );
    let with_key = stderr_of_failure(
        &run_cli(
            &rig.url,
            home.path(),
            &["infer", "--dry-run", "--idempotency-key", "k", "q"],
        )
        .await,
    );
    assert!(
        with_key.contains("dry-run") && with_key.contains("idempotency-key"),
        "dry-run + key is refused, naming both: {with_key}"
    );
}

#[tokio::test]
async fn a_dry_run_with_no_capable_operator_refuses_and_dispatches_nothing() {
    let rig = Rig::launch().await;
    let home = tempfile::tempdir().unwrap();

    // The rig node serves "any" model but no operator can meet a 999 GB
    // VRAM floor, so the preview refuses with the same "nothing was
    // dispatched" message a real buy would, before any spend.
    let refusal = stderr_of_failure(
        &run_cli(
            &rig.url,
            home.path(),
            &["infer", "--dry-run", "--min-vram-gb", "999", "q"],
        )
        .await,
    );
    assert!(
        refusal.contains("no operator is serving") && refusal.contains("nothing was dispatched"),
        "an impossible ask refuses in preview, dispatching nothing: {refusal}"
    );
}

#[tokio::test]
async fn the_cli_opens_views_and_closes_a_lease_end_to_end() {
    let rig = Rig::launch_leasing().await;
    let home = tempfile::tempdir().unwrap();
    let buyer =
        LocalIdentity::load_or_create(&home.path().join("identity.json"), "buyer@compute").unwrap();
    rig.rail.preload(VerifiedDeposit {
        deposit_id: "lease-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });
    run_cli(&rig.url, home.path(), &["deposit", "lease-deposit"]).await;

    // The buyer's own ssh public key, from a file — the way a person points
    // --ssh-key at ~/.ssh/id_ed25519.pub.
    let key_path = home.path().join("id_ed25519.pub");
    std::fs::write(
        &key_path,
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITestKeyForCliLease buyer@cli\n",
    )
    .unwrap();

    // Open a one-minute lease at 200 micro-USDC/s: the operator's flat ask
    // is 10_000, so the 12_000 window ceiling clears the floor. The stub
    // node publishes its endpoint at once, so the wait resolves fast.
    let opened = run_cli(
        &rig.url,
        home.path(),
        &[
            "lease",
            "open",
            "--minutes",
            "1",
            "--rate",
            "200",
            "--ssh-key",
            key_path.to_str().unwrap(),
            "--wait-secs",
            "15",
            "--json",
        ],
    )
    .await;
    assert!(
        opened.status.success(),
        "lease open succeeds: {}",
        stderr_of_failure(&opened)
    );
    let view: serde_json::Value = serde_json::from_str(&stdout_of(&opened)).unwrap();
    let job_id = view["job_id"].as_str().expect("a job id").to_string();
    assert_eq!(
        view["access"]["endpoint"], "ssh renter@stub.test -p 2222",
        "the buyer is handed the machine's address: {view}"
    );
    assert_eq!(view["status"], "accepted", "the session is running: {view}");
    assert_eq!(view["close_requested"], false);
    assert_eq!(view["rate_micro_usdc_per_sec"], 200);
    assert_eq!(view["max_duration_secs"], 60);

    // The human view names the endpoint and the meter.
    let human = stdout_of(&run_cli(&rig.url, home.path(), &["lease", "view", &job_id]).await);
    assert!(
        human.contains("reach it:") && human.contains("ssh renter@stub.test -p 2222"),
        "the human view shows how to reach the machine: {human}"
    );
    assert!(
        human.contains("200 micro-USDC/s"),
        "the human view shows the rate: {human}"
    );

    // Close it: the close is recorded synchronously, the meter stops.
    let closed = run_cli(
        &rig.url,
        home.path(),
        &["lease", "close", &job_id, "--json"],
    )
    .await;
    assert!(
        closed.status.success(),
        "lease close succeeds: {}",
        stderr_of_failure(&closed)
    );
    let closed_view: serde_json::Value = serde_json::from_str(&stdout_of(&closed)).unwrap();
    assert_eq!(
        closed_view["close_requested"], true,
        "the close is recorded: {closed_view}"
    );

    // The node sees the close, releases the session and submits its
    // receipt, which settles the meter to the seconds actually served —
    // well under the full-window ceiling.
    let mut settled = None;
    for _ in 0..100 {
        let v: serde_json::Value = serde_json::from_str(&stdout_of(
            &run_cli(&rig.url, home.path(), &["lease", "view", &job_id, "--json"]).await,
        ))
        .unwrap();
        if v["status"] == "completed" {
            settled = Some(v);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let settled = settled.expect("the lease settles to completed after the close");
    let charged = settled["charged_micro_usdc"].as_u64().unwrap();
    assert!(
        charged < 12_000,
        "an early close bills only the seconds served, not the whole window: {settled}"
    );

    // Closing again is an honest no-op, not an error — the meter already
    // stopped, so a retry answers the fact.
    let reclose = run_cli(&rig.url, home.path(), &["lease", "close", &job_id]).await;
    assert!(
        reclose.status.success(),
        "re-closing a settled lease is safe: {}",
        stderr_of_failure(&reclose)
    );
}

#[tokio::test]
async fn lease_open_needs_a_window_and_a_rate() {
    let rig = Rig::launch_leasing().await;
    let home = tempfile::tempdir().unwrap();

    let no_window = stderr_of_failure(
        &run_cli(&rig.url, home.path(), &["lease", "open", "--rate", "100"]).await,
    );
    assert!(
        no_window.contains("needs a window"),
        "a lease with no window is refused: {no_window}"
    );

    let no_rate = stderr_of_failure(
        &run_cli(&rig.url, home.path(), &["lease", "open", "--minutes", "1"]).await,
    );
    assert!(
        no_rate.contains("--rate"),
        "a lease with no rate is refused: {no_rate}"
    );

    let no_action = stderr_of_failure(&run_cli(&rig.url, home.path(), &["lease"]).await);
    assert!(
        no_action.contains("open") && no_action.contains("view") && no_action.contains("close"),
        "a bare lease names its actions: {no_action}"
    );
}

/// A coordinator serving the client-sealed vault and nothing else — the
/// vault needs no node, no rail and no money path. Returns the base URL
/// and the home its append-only log lives in, so a test can prove the
/// coordinator only ever holds ciphertext.
async fn launch_vault_coordinator() -> (String, tempfile::TempDir) {
    let home = tempfile::tempdir().unwrap();
    let state = CoordinatorState::with_journal(
        LocalIdentity::generate("coordinator@test"),
        CoordinatorConfig {
            vault_enabled: true,
            ..CoordinatorConfig::default()
        },
        Arc::new(NoReputation),
        Arc::new(MockPayout::new()),
        Arc::new(covenant_audit::InMemoryAuditLog::new()),
        &home.path().join("journal.jsonl"),
        None,
    )
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, compute_router(state)).await.unwrap();
    });
    (url, home)
}

#[tokio::test]
async fn the_vault_cli_seals_stores_and_opens_a_secret_end_to_end() {
    let (url, coord_home) = launch_vault_coordinator().await;
    let home = tempfile::tempdir().unwrap();

    let secret = "sk-live-SECRET-do-not-log-xyz";
    let put = stdout_of(&run_cli(&url, home.path(), &["vault", "put", "deploy", secret]).await);
    assert!(put.contains("stored 'deploy'"), "put confirms: {put}");
    assert!(
        put.contains("Back it up"),
        "the first put reminds the buyer to back up the new key: {put}"
    );

    let ls = stdout_of(&run_cli(&url, home.path(), &["vault", "ls"]).await);
    assert!(
        ls.contains("deploy") && ls.contains("openable"),
        "ls shows the secret and marks it openable here: {ls}"
    );

    let got = run_cli(&url, home.path(), &["vault", "get", "deploy"]).await;
    assert!(
        got.status.success(),
        "get succeeds: {}",
        stderr_of_failure(&got)
    );
    assert_eq!(
        String::from_utf8_lossy(&got.stdout),
        secret,
        "get returns the sealed plaintext byte for byte"
    );

    // The coordinator's own store holds ciphertext, never the plaintext.
    let vault_log = std::fs::read_to_string(coord_home.path().join("vault.jsonl")).unwrap();
    assert!(
        vault_log.contains("deploy") && vault_log.contains("ciphertext"),
        "the label and a ciphertext field are stored: {vault_log}"
    );
    assert!(
        !vault_log.contains(secret),
        "the plaintext is never written to the coordinator: {vault_log}"
    );

    // A binary secret round-trips through --file and raw stdout.
    let bin_path = home.path().join("blob.bin");
    let blob: &[u8] = &[0, 159, 146, 150, 255, 10, 0, 7];
    std::fs::write(&bin_path, blob).unwrap();
    let put_bin = run_cli(
        &url,
        home.path(),
        &["vault", "put", "blob", "--file", bin_path.to_str().unwrap()],
    )
    .await;
    assert!(
        put_bin.status.success(),
        "a binary put succeeds: {}",
        stderr_of_failure(&put_bin)
    );
    let got_bin = run_cli(&url, home.path(), &["vault", "get", "blob"]).await;
    assert_eq!(
        got_bin.stdout, blob,
        "a binary secret round-trips byte for byte"
    );

    // rm deletes the secret with the coordinator; ls no longer lists it.
    let rm = stdout_of(&run_cli(&url, home.path(), &["vault", "rm", "deploy"]).await);
    assert!(rm.contains("deleted 'deploy'"), "rm confirms: {rm}");
    let ls_after = stdout_of(&run_cli(&url, home.path(), &["vault", "ls"]).await);
    assert!(
        !ls_after.contains("deploy"),
        "the deleted secret is gone from the coordinator listing: {ls_after}"
    );
}

#[tokio::test]
async fn the_vault_keyring_moves_a_secret_to_another_machine() {
    let (url, _coord_home) = launch_vault_coordinator().await;
    let machine_a = tempfile::tempdir().unwrap();

    let secret = "api-token-42";
    let _ = stdout_of(&run_cli(&url, machine_a.path(), &["vault", "put", "shared", secret]).await);
    let key = stdout_of(
        &run_cli(
            &url,
            machine_a.path(),
            &["vault", "key", "export", "shared"],
        )
        .await,
    );
    let key = key.trim().to_string();

    // A second machine carrying the same buyer identity, but no key, sees
    // the stored secret yet cannot open it.
    let machine_b = tempfile::tempdir().unwrap();
    std::fs::copy(
        machine_a.path().join("identity.json"),
        machine_b.path().join("identity.json"),
    )
    .unwrap();
    let blind = run_cli(&url, machine_b.path(), &["vault", "get", "shared"]).await;
    assert!(
        !blind.status.success(),
        "machine B cannot open the secret without the key"
    );
    assert!(
        stderr_of_failure(&blind).contains("no local key"),
        "and says why: {}",
        stderr_of_failure(&blind)
    );
    let ls_b = stdout_of(&run_cli(&url, machine_b.path(), &["vault", "ls"]).await);
    assert!(
        ls_b.contains("shared") && ls_b.contains("NO LOCAL KEY"),
        "B sees it stored but flags it unopenable: {ls_b}"
    );

    // Importing the exported key lets B open the very same secret.
    let import = run_cli(
        &url,
        machine_b.path(),
        &["vault", "key", "import", "shared", &key],
    )
    .await;
    assert!(
        import.status.success(),
        "import succeeds: {}",
        stderr_of_failure(&import)
    );
    let got_b = run_cli(&url, machine_b.path(), &["vault", "get", "shared"]).await;
    assert_eq!(
        String::from_utf8_lossy(&got_b.stdout),
        secret,
        "B now opens the same secret A stored"
    );

    // Replacing that key with a different one needs --force.
    let _ = stdout_of(&run_cli(&url, machine_b.path(), &["vault", "put", "throwaway", "x"]).await);
    let other = stdout_of(
        &run_cli(
            &url,
            machine_b.path(),
            &["vault", "key", "export", "throwaway"],
        )
        .await,
    );
    let other = other.trim().to_string();
    let refuse = run_cli(
        &url,
        machine_b.path(),
        &["vault", "key", "import", "shared", &other],
    )
    .await;
    assert!(
        !refuse.status.success(),
        "replacing a differing key is refused without --force"
    );
    assert!(
        stderr_of_failure(&refuse).contains("--force"),
        "and points at --force: {}",
        stderr_of_failure(&refuse)
    );
    let forced = run_cli(
        &url,
        machine_b.path(),
        &["vault", "key", "import", "shared", &other, "--force"],
    )
    .await;
    assert!(
        forced.status.success(),
        "--force replaces the key: {}",
        stderr_of_failure(&forced)
    );
}
