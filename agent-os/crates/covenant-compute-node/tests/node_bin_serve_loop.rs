//! The `covenant-compute-node` binary end to end: a real spawned
//! operator process configured through its environment against a real
//! coordinator — boot, the benchmark-before-register gate, a paid job
//! through the echo executor, the SIGTERM drain, and the `earnings`
//! subcommand reading the home the serve life wrote. The hermetic twin
//! of the manual live-node proof.

#![cfg(unix)]

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use covenant_compute_buyer::{
    claim_deposit, dispatch_and_verify, dispatch_streaming, BuyerConfig, JobRequest,
};
use covenant_compute_coordinator::{
    router as compute_router, CoordinatorConfig, CoordinatorState, MockPayout, MockRail,
    NoReputation, Payout, PayoutError, PayoutRecord, TransferRecord, VerifiedBond, VerifiedDeposit,
};
use covenant_compute_protocol::{
    parse_embedding_output, parse_speech_output, parse_transcription_output, speech_input,
    transcription_input, FundingSource, JobKind, SignedWorkReceipt, SpeechInput,
    TranscriptionInput,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use uuid::Uuid;

async fn coordinator() -> (String, String, Arc<MockRail>, tempfile::TempDir) {
    coordinator_with_payout(Arc::new(MockPayout::new())).await
}

async fn coordinator_with_payout(
    payout: Arc<dyn Payout>,
) -> (String, String, Arc<MockRail>, tempfile::TempDir) {
    let home = tempfile::tempdir().unwrap();
    let rail = Arc::new(MockRail::new());
    let state = CoordinatorState::with_journal(
        LocalIdentity::generate("coordinator@test"),
        CoordinatorConfig {
            // Short long-poll so the drain isn't stuck behind an idle
            // work poll.
            long_poll_timeout: Duration::from_secs(1),
            default_funding_source: FundingSource::Organic,
            ..CoordinatorConfig::default()
        },
        Arc::new(NoReputation),
        payout,
        Arc::new(covenant_audit::InMemoryAuditLog::new()),
        &home.path().join("journal.jsonl"),
        Some(rail.clone()),
    )
    .await
    .unwrap();
    let pubkey = state.coordinator_pubkey_b58();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, compute_router(state)).await.unwrap();
    });
    (url, pubkey, rail, home)
}

/// A payout backend that reports an on-chain submission: same instant
/// settlement as `MockPayout`, but every record carries a transaction
/// signature — the ledger shape a node sees behind a real rail.
struct StampingPayout;

#[async_trait::async_trait]
impl Payout for StampingPayout {
    async fn pay(
        &self,
        job_id: Uuid,
        operator_pubkey_b58: &str,
        payout_address: &str,
        amount_micro_usdc: u64,
        _receipt: &SignedWorkReceipt,
    ) -> Result<PayoutRecord, PayoutError> {
        Ok(PayoutRecord {
            job_id,
            operator_pubkey_b58: operator_pubkey_b58.into(),
            payout_address: payout_address.into(),
            amount_micro_usdc,
            recorded_at_ms: 0,
            tx_signature: Some(format!("stamped-{job_id}")),
        })
    }

    async fn transfer(
        &self,
        transfer_id: Uuid,
        recipient_address: &str,
        amount_micro_usdc: u64,
        _memo: &str,
    ) -> Result<TransferRecord, PayoutError> {
        Ok(TransferRecord {
            transfer_id,
            recipient_address: recipient_address.into(),
            amount_micro_usdc,
            recorded_at_ms: 0,
            tx_signature: None,
        })
    }
}

/// The operator's real invocation: no arguments, everything through
/// the environment, from a clean slate.
fn spawn_node(coordinator_url: &str, coordinator_pubkey: &str, home: &std::path::Path) -> Child {
    spawn_node_with(coordinator_url, coordinator_pubkey, home, &[])
}

/// A well-formed 32-byte base58 payout address — boots validate the
/// shape, and the payout backend in these tests is a mock, so the
/// bytes themselves are inert.
fn test_payout_address() -> String {
    bs58::encode([7u8; 32]).into_string()
}

/// A well-formed coordinator pubkey for boot tests that fail before any
/// coordinator traffic: never the real key, but valid base58/32 bytes so
/// the boot's own format guard passes and the test reaches the failure it
/// is actually exercising.
fn test_coordinator_pubkey() -> String {
    bs58::encode([9u8; 32]).into_string()
}

fn spawn_node_with(
    coordinator_url: &str,
    coordinator_pubkey: &str,
    home: &std::path::Path,
    extra_env: &[(&str, &str)],
) -> Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"));
    cmd.env_clear()
        .env("COVENANT_COMPUTE_NODE_HOME", home)
        .env("COVENANT_COMPUTE_COORDINATOR_URL", coordinator_url)
        .env("COVENANT_COMPUTE_COORDINATOR_PUBKEY", coordinator_pubkey)
        .env("COVENANT_COMPUTE_PAYOUT_ADDRESS", test_payout_address())
        .env("COVENANT_COMPUTE_NODE_EXECUTOR", "echo")
        .env("COVENANT_COMPUTE_NODE_JOB_KINDS", "inference_call")
        .env("COVENANT_COMPUTE_NODE_HEARTBEAT_SECS", "1")
        .env("COVENANT_COMPUTE_NODE_PAYOUT_POLL_SECS", "1");
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenant-compute-node")
}

/// Boots the binary purely from the `node.env` a `setup` run wrote: no
/// executor, job-kinds, or trust anchors in the environment, so the
/// wizard's own file is the whole configuration. `PATH` is passed through
/// because `env_clear` strips it and a `say`/whisper backend resolves its
/// binary there; the fast heartbeat and payout cadence just keep the test
/// brisk.
fn spawn_node_from_home(home: &std::path::Path, path_env: &str) -> Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"));
    cmd.env_clear()
        .env("COVENANT_COMPUTE_NODE_HOME", home)
        .env("COVENANT_COMPUTE_NODE_HEARTBEAT_SECS", "1")
        .env("COVENANT_COMPUTE_NODE_PAYOUT_POLL_SECS", "1")
        .env("PATH", path_env);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenant-compute-node")
}

/// Registration is the binary's own act here — observe it the way an
/// operator would, on the coordinator's public metrics.
async fn wait_registered(coordinator_url: &str) {
    wait_registered_count(coordinator_url, 1).await;
}

async fn wait_registered_count(coordinator_url: &str, count: usize) {
    let http = reqwest::Client::new();
    for _ in 0..300 {
        if let Ok(resp) = http.get(format!("{coordinator_url}/metrics")).send().await {
            if let Ok(body) = resp.text().await {
                if body.contains(&format!("\ncompute_operators_registered {count}")) {
                    return;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the node never registered");
}

#[tokio::test]
async fn the_node_binary_boots_benchmarks_serves_a_paid_job_and_drains_clean_on_sigterm() {
    let (url, coordinator_pubkey, rail, _coordinator_home) = coordinator().await;
    let node_home = tempfile::tempdir().unwrap();
    let mut node = spawn_node(&url, &coordinator_pubkey, node_home.path());
    wait_registered(&url).await;

    // B5 ran before the registration we just observed, and its probe
    // landed in the node's own audit chain.
    let audit = std::fs::read_to_string(node_home.path().join("audit.jsonl")).unwrap();
    let probe = audit
        .lines()
        .find(|l| l.contains("compute_capability_benchmarked"))
        .expect("the benchmark probe is on the audit chain");
    assert!(probe.contains("\"passed\":true"), "got: {probe}");

    // A funded buyer purchases one inference job from the running
    // process.
    let http = reqwest::Client::new();
    let config = BuyerConfig {
        coordinator_url: url.clone(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    let buyer = LocalIdentity::generate("buyer@test");
    rail.preload(VerifiedDeposit {
        deposit_id: "node-bin-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });
    let claimed = claim_deposit(&http, &config, &buyer, "node-bin-deposit")
        .await
        .unwrap();
    assert!(claimed.credited);

    let outcome = dispatch_and_verify(
        &http,
        &config,
        &buyer,
        JobRequest {
            kind: JobKind::InferenceCall,
            input: vec![Content::text("the node binary question")],
            model: None,
            gpu_class: None,
            min_vram_gb: None,
            min_reputation_bps: None,
            price_micro_usdc: 25_000,
            deadline_ms: 30_000,
        },
    )
    .await
    .expect("the spawned node serves the job");
    match &outcome.output[0] {
        Content::Text { text } => assert_eq!(text, "the node binary question"),
        other => panic!("echoed text expected, got {other:?}"),
    }
    let receipt = &outcome.receipt.receipt;
    assert_eq!(receipt.price_micro_usdc, 25_000);
    let job_id = receipt.job_id.to_string();

    // The serve life journals its side of the money into the home
    // before the operator ever runs a CLI read.
    let earnings_path = node_home.path().join("earnings.jsonl");
    let mut credited = String::new();
    for _ in 0..100 {
        credited = std::fs::read_to_string(&earnings_path).unwrap_or_default();
        if credited.contains(&job_id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        credited.contains(&job_id),
        "the served job is on the earnings ledger"
    );

    // SIGTERM = drain: finish in-flight work, take nothing new, exit 0.
    let pid = node.id().expect("serving child has a pid");
    let killed = std::process::Command::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let status = tokio::time::timeout(Duration::from_secs(15), node.wait())
        .await
        .expect("drained exit within 15s of SIGTERM")
        .expect("wait on node");
    assert!(status.success(), "a clean drain exits 0, got {status}");

    // A later life reads the same home: the `earnings` subcommand
    // reports the credited job without touching the coordinator.
    let report = Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"))
        .env_clear()
        .env("COVENANT_COMPUTE_NODE_HOME", node_home.path())
        .arg("earnings")
        .output()
        .await
        .unwrap();
    assert!(report.status.success(), "earnings read exits 0");
    let text = String::from_utf8_lossy(&report.stdout);
    assert!(text.contains("jobs credited:"), "got: {text}");
    assert!(text.contains("25000 micro-USDC"), "got: {text}");
}

/// The supply side proven with real isolation, not the echo stand-in: the
/// real binary configured for the container executor boots, runs its
/// benchmark inside a real container, registers, serves a paid batch job
/// whose command runs sandboxed, and credits the earnings. Ignored by
/// default (needs docker and pulls `alpine:3.20`); run with `--ignored`.
#[tokio::test]
#[ignore = "requires a working docker engine and pulls alpine:3.20"]
async fn the_node_binary_serves_a_paid_batch_job_in_a_real_container() {
    let (url, coordinator_pubkey, rail, _coordinator_home) = coordinator().await;
    let node_home = tempfile::tempdir().unwrap();
    // env_clear() strips PATH and HOME; the container executor needs the
    // former to find `docker` and the latter for the CLI's daemon context.
    let path = std::env::var("PATH").unwrap_or_default();
    let host_home = std::env::var("HOME").unwrap_or_default();
    let mut node = spawn_node_with(
        &url,
        &coordinator_pubkey,
        node_home.path(),
        &[
            ("COVENANT_COMPUTE_NODE_EXECUTOR", "container"),
            ("COVENANT_COMPUTE_NODE_JOB_KINDS", "batch_job"),
            ("COVENANT_COMPUTE_NODE_CONTAINER_IMAGE", "alpine:3.20"),
            ("PATH", &path),
            ("HOME", &host_home),
        ],
    );
    wait_registered(&url).await;

    // The benchmark ran inside a real container (`printf %s <nonce>`) and
    // passed before the node registered.
    let audit = std::fs::read_to_string(node_home.path().join("audit.jsonl")).unwrap();
    let probe = audit
        .lines()
        .find(|l| l.contains("compute_capability_benchmarked"))
        .expect("the benchmark probe is on the audit chain");
    assert!(probe.contains("\"passed\":true"), "got: {probe}");

    let http = reqwest::Client::new();
    let config = BuyerConfig {
        coordinator_url: url.clone(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    let buyer = LocalIdentity::generate("buyer@test");
    rail.preload(VerifiedDeposit {
        deposit_id: "batch-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });
    assert!(
        claim_deposit(&http, &config, &buyer, "batch-deposit")
            .await
            .unwrap()
            .credited
    );

    let marker = "compute-batch-marker";
    let outcome = dispatch_and_verify(
        &http,
        &config,
        &buyer,
        JobRequest {
            kind: JobKind::BatchJob,
            input: vec![Content::text(format!("echo {marker}"))],
            model: None,
            gpu_class: None,
            min_vram_gb: None,
            min_reputation_bps: None,
            price_micro_usdc: 25_000,
            deadline_ms: 30_000,
        },
    )
    .await
    .expect("the spawned container node serves the batch job");
    match &outcome.output[0] {
        Content::Text { text } => assert!(
            text.contains(marker),
            "the container's stdout should carry the command output, got {text:?}"
        ),
        other => panic!("text output expected, got {other:?}"),
    }
    let job_id = outcome.receipt.receipt.job_id.to_string();

    let earnings_path = node_home.path().join("earnings.jsonl");
    let mut credited = String::new();
    for _ in 0..100 {
        credited = std::fs::read_to_string(&earnings_path).unwrap_or_default();
        if credited.contains(&job_id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        credited.contains(&job_id),
        "the served batch job is on the earnings ledger"
    );

    let pid = node.id().expect("serving child has a pid");
    let killed = std::process::Command::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let status = tokio::time::timeout(Duration::from_secs(15), node.wait())
        .await
        .expect("drained exit within 15s of SIGTERM")
        .expect("wait on node");
    assert!(status.success(), "a clean drain exits 0, got {status}");
}

/// The flagship inference executor proven against a real engine, not a
/// mock HTTP stand-in: the real binary configured for `ollama` boots,
/// runs its benchmark-before-register against a live Ollama, registers,
/// serves a paid `InferenceCall` that qwen2.5:0.5b actually generates,
/// and credits the earnings. The metering is real too — the receipt
/// carries the token counts Ollama reported for the generation, the one
/// thing a mocked backend can never prove. Ignored by default (needs a
/// running Ollama serving the model); run with `--ignored`.
#[tokio::test]
#[ignore = "requires a running ollama at 127.0.0.1:11434 serving qwen2.5:0.5b"]
async fn the_node_binary_serves_a_paid_inference_job_through_real_ollama() {
    let (url, coordinator_pubkey, rail, _coordinator_home) = coordinator().await;
    let node_home = tempfile::tempdir().unwrap();
    let mut node = spawn_node_with(
        &url,
        &coordinator_pubkey,
        node_home.path(),
        &[
            ("COVENANT_COMPUTE_NODE_EXECUTOR", "ollama"),
            ("COVENANT_COMPUTE_NODE_JOB_KINDS", "inference_call"),
            ("COVENANT_COMPUTE_OLLAMA_URL", "http://127.0.0.1:11434"),
            ("COVENANT_COMPUTE_NODE_MODELS", "qwen2.5:0.5b"),
        ],
    );
    wait_registered(&url).await;

    // The benchmark ran a real generation against Ollama and passed
    // before the node registered — a generative backend's probe asserts
    // non-empty output, not an exact token match.
    let audit = std::fs::read_to_string(node_home.path().join("audit.jsonl")).unwrap();
    let probe = audit
        .lines()
        .find(|l| l.contains("compute_capability_benchmarked"))
        .expect("the benchmark probe is on the audit chain");
    assert!(probe.contains("\"passed\":true"), "got: {probe}");

    let http = reqwest::Client::new();
    let config = BuyerConfig {
        coordinator_url: url.clone(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    let buyer = LocalIdentity::generate("buyer@test");
    rail.preload(VerifiedDeposit {
        deposit_id: "ollama-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });
    assert!(
        claim_deposit(&http, &config, &buyer, "ollama-deposit")
            .await
            .unwrap()
            .credited
    );

    let outcome = dispatch_and_verify(
        &http,
        &config,
        &buyer,
        JobRequest {
            kind: JobKind::InferenceCall,
            input: vec![Content::text(
                "Reply with a short greeting in one sentence.",
            )],
            model: Some("qwen2.5:0.5b".into()),
            gpu_class: None,
            min_vram_gb: None,
            min_reputation_bps: None,
            price_micro_usdc: 25_000,
            deadline_ms: 30_000,
        },
    )
    .await
    .expect("the spawned ollama node serves the inference job");
    match &outcome.output[0] {
        Content::Text { text } => assert!(
            !text.trim().is_empty(),
            "a real generation returns non-empty text"
        ),
        other => panic!("text output expected, got {other:?}"),
    }

    // The receipt bills what the model actually did: Ollama's
    // prompt/eval token counts, relayed through the signed meter.
    let receipt = &outcome.receipt.receipt;
    assert_eq!(receipt.price_micro_usdc, 25_000);
    assert!(
        receipt.meter.tokens_in.unwrap_or(0) > 0,
        "real prompt-token metering, got {:?}",
        receipt.meter.tokens_in
    );
    assert!(
        receipt.meter.tokens_out.unwrap_or(0) > 0,
        "real completion-token metering, got {:?}",
        receipt.meter.tokens_out
    );
    let job_id = receipt.job_id.to_string();

    let earnings_path = node_home.path().join("earnings.jsonl");
    let mut credited = String::new();
    for _ in 0..100 {
        credited = std::fs::read_to_string(&earnings_path).unwrap_or_default();
        if credited.contains(&job_id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        credited.contains(&job_id),
        "the served inference job is on the earnings ledger"
    );

    let pid = node.id().expect("serving child has a pid");
    let killed = std::process::Command::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let status = tokio::time::timeout(Duration::from_secs(15), node.wait())
        .await
        .expect("drained exit within 15s of SIGTERM")
        .expect("wait on node");
    assert!(status.success(), "a clean drain exits 0, got {status}");
}

/// The embedding path proven against a real engine: an `embedding`
/// profile benchmarks with a real vector (not a chat probe), registers,
/// and serves a paid `Embedding` job whose output is the actual vector
/// nomic-embed-text produces over `/api/embed`. Ignored by default (needs
/// a running Ollama serving the embedding model); run with `--ignored`.
#[tokio::test]
#[ignore = "requires a running ollama at 127.0.0.1:11434 serving nomic-embed-text"]
async fn the_node_binary_serves_a_paid_embedding_job_through_real_ollama() {
    let (url, coordinator_pubkey, rail, _coordinator_home) = coordinator().await;
    let node_home = tempfile::tempdir().unwrap();
    let mut node = spawn_node_with(
        &url,
        &coordinator_pubkey,
        node_home.path(),
        &[
            ("COVENANT_COMPUTE_NODE_EXECUTOR", "ollama"),
            ("COVENANT_COMPUTE_NODE_JOB_KINDS", "embedding"),
            ("COVENANT_COMPUTE_OLLAMA_URL", "http://127.0.0.1:11434"),
            ("COVENANT_COMPUTE_NODE_MODELS", "nomic-embed-text"),
        ],
    );
    wait_registered(&url).await;

    // The benchmark embedded a real probe (an embedding model can't pass a
    // generation probe) and passed before the node registered.
    let audit = std::fs::read_to_string(node_home.path().join("audit.jsonl")).unwrap();
    let probe = audit
        .lines()
        .find(|l| l.contains("compute_capability_benchmarked"))
        .expect("the benchmark probe is on the audit chain");
    assert!(probe.contains("\"passed\":true"), "got: {probe}");

    let http = reqwest::Client::new();
    let config = BuyerConfig {
        coordinator_url: url.clone(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    let buyer = LocalIdentity::generate("buyer@test");
    rail.preload(VerifiedDeposit {
        deposit_id: "embed-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });
    assert!(
        claim_deposit(&http, &config, &buyer, "embed-deposit")
            .await
            .unwrap()
            .credited
    );

    let outcome = dispatch_and_verify(
        &http,
        &config,
        &buyer,
        JobRequest {
            kind: JobKind::Embedding,
            input: vec![Content::text("a sentence to embed")],
            model: Some("nomic-embed-text".into()),
            gpu_class: None,
            min_vram_gb: None,
            min_reputation_bps: None,
            price_micro_usdc: 25_000,
            deadline_ms: 30_000,
        },
    )
    .await
    .expect("the spawned ollama node serves the embedding job");

    // The output is the real vector: one per input, non-empty, its stated
    // width matching the data, every component finite.
    let embedding = parse_embedding_output(&outcome.output).expect("output is an embedding result");
    assert_eq!(embedding.embeddings.len(), 1, "one vector per input");
    let vector = &embedding.embeddings[0];
    assert!(!vector.is_empty(), "a real embedding is non-empty");
    assert_eq!(
        embedding.dimensions,
        vector.len(),
        "the stated width matches the vector"
    );
    assert!(
        vector.iter().all(|c| c.is_finite()),
        "every component is finite"
    );
    assert!(
        outcome.receipt.receipt.meter.tokens_in.unwrap_or(0) > 0,
        "real prompt-token metering on the embed"
    );

    let pid = node.id().expect("serving child has a pid");
    assert!(std::process::Command::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap()
        .success());
    let status = tokio::time::timeout(Duration::from_secs(15), node.wait())
        .await
        .expect("drained exit within 15s of SIGTERM")
        .expect("wait on node");
    assert!(status.success(), "a clean drain exits 0, got {status}");
}

/// The transcription path proven against a real whisper.cpp install: a
/// `whisper` node benchmarks with a real transcript of the bundled clip
/// (not a chat or embedding probe), registers, and serves a paid
/// `Transcription` job whose output is the actual text whisper hears in the
/// buyer's audio. Ignored by default (needs whisper-cli and a ggml model);
/// point `COVENANT_COMPUTE_TEST_WHISPER_MODEL` at a `ggml-*.bin` (and
/// `_BIN` at the CLI, since the spawned node has no PATH) and run with
/// `--ignored`.
#[tokio::test]
#[ignore = "requires whisper-cli and a ggml model; set COVENANT_COMPUTE_TEST_WHISPER_MODEL"]
async fn the_node_binary_serves_a_paid_transcription_job_through_real_whisper() {
    let model = std::env::var("COVENANT_COMPUTE_TEST_WHISPER_MODEL")
        .expect("set COVENANT_COMPUTE_TEST_WHISPER_MODEL to a ggml-*.bin path");
    let binary =
        std::env::var("COVENANT_COMPUTE_TEST_WHISPER_BIN").unwrap_or_else(|_| "whisper-cli".into());
    let (url, coordinator_pubkey, rail, _coordinator_home) = coordinator().await;
    let node_home = tempfile::tempdir().unwrap();
    let mut node = spawn_node_with(
        &url,
        &coordinator_pubkey,
        node_home.path(),
        &[
            ("COVENANT_COMPUTE_NODE_EXECUTOR", "whisper"),
            ("COVENANT_COMPUTE_NODE_JOB_KINDS", "transcription"),
            ("COVENANT_COMPUTE_WHISPER_MODEL", model.as_str()),
            ("COVENANT_COMPUTE_WHISPER_BIN", binary.as_str()),
        ],
    );
    wait_registered(&url).await;

    // The benchmark ran a real transcription probe (a speech model passes
    // neither a chat nor an embedding probe) and passed before registering.
    let audit = std::fs::read_to_string(node_home.path().join("audit.jsonl")).unwrap();
    let probe = audit
        .lines()
        .find(|l| l.contains("compute_capability_benchmarked"))
        .expect("the benchmark probe is on the audit chain");
    assert!(probe.contains("\"passed\":true"), "got: {probe}");

    let http = reqwest::Client::new();
    let config = BuyerConfig {
        coordinator_url: url.clone(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    let buyer = LocalIdentity::generate("buyer@test");
    rail.preload(VerifiedDeposit {
        deposit_id: "whisper-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });
    assert!(
        claim_deposit(&http, &config, &buyer, "whisper-deposit")
            .await
            .unwrap()
            .credited
    );

    let audio = base64::engine::general_purpose::STANDARD
        .encode(include_bytes!("../src/whisper_benchmark.wav"));
    let outcome = dispatch_and_verify(
        &http,
        &config,
        &buyer,
        JobRequest {
            kind: JobKind::Transcription,
            input: transcription_input(TranscriptionInput::new(audio)),
            model: Some("whisper-1".into()),
            gpu_class: None,
            min_vram_gb: None,
            min_reputation_bps: None,
            price_micro_usdc: 25_000,
            deadline_ms: 30_000,
        },
    )
    .await
    .expect("the spawned whisper node serves the transcription job");

    // The receipt settled over the real transcript: the bundled clip speaks
    // "covenant compute", so the paid-for output carries those words.
    let result =
        parse_transcription_output(&outcome.output).expect("output is a transcription result");
    assert!(
        result
            .transcript
            .to_ascii_lowercase()
            .contains("covenant compute"),
        "expected the spoken words in the paid transcript, got {:?}",
        result.transcript
    );
    assert_eq!(result.model, "whisper-1");

    let pid = node.id().expect("serving child has a pid");
    assert!(std::process::Command::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap()
        .success());
    let status = tokio::time::timeout(Duration::from_secs(15), node.wait())
        .await
        .expect("drained exit within 15s of SIGTERM")
        .expect("wait on node");
    assert!(status.success(), "a clean drain exits 0, got {status}");
}

/// The whole supply side for a text-to-speech operator, end to end and
/// non-developer-shaped: the `setup` wizard writes a say node.env, the real
/// binary boots from exactly that file with no executor in its environment,
/// benchmarks synthesis, registers, serves a paid `SpeechSynthesis` job that
/// the local synthesizer actually voices, and credits the earnings. This is
/// the proof the wizard writes a config that truly boots and earns, not one
/// that only parses. Ignored by default (needs the macOS `say` binary on
/// PATH); run with `--ignored`.
#[tokio::test]
#[ignore = "requires the macOS `say` binary on PATH"]
async fn a_say_node_onboarded_by_the_wizard_boots_serves_and_earns() {
    let (url, coordinator_pubkey, rail, _coordinator_home) = coordinator().await;
    let node_home = tempfile::tempdir().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();

    // The exact file `covenant-compute-node setup --executor say ...`
    // produces, written by the wizard itself rather than a hand-set env.
    let opts = covenant_compute_node::SetupOptions {
        coordinator_url: Some(url.clone()),
        coordinator_pubkey_b58: Some(coordinator_pubkey.clone()),
        payout_address: Some(test_payout_address()),
        executor: Some("say".into()),
        price_micro_usdc: Some(1_000),
        ..Default::default()
    };
    let mut wizard_out = Vec::new();
    covenant_compute_node::run_setup(
        node_home.path(),
        opts,
        None,
        &mut std::io::Cursor::new(String::new()),
        &mut wizard_out,
    )
    .await
    .expect("the say wizard writes a node.env");

    let mut node = spawn_node_from_home(node_home.path(), &path);
    wait_registered(&url).await;

    // The say benchmark voiced a line through the real backend and passed on
    // a non-empty clip before the node registered.
    let audit = std::fs::read_to_string(node_home.path().join("audit.jsonl")).unwrap();
    let probe = audit
        .lines()
        .find(|l| l.contains("compute_capability_benchmarked"))
        .expect("the benchmark probe is on the audit chain");
    assert!(probe.contains("\"passed\":true"), "got: {probe}");

    let http = reqwest::Client::new();
    let config = BuyerConfig {
        coordinator_url: url.clone(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    let buyer = LocalIdentity::generate("buyer@test");
    rail.preload(VerifiedDeposit {
        deposit_id: "say-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });
    assert!(
        claim_deposit(&http, &config, &buyer, "say-deposit")
            .await
            .unwrap()
            .credited
    );

    let outcome = dispatch_and_verify(
        &http,
        &config,
        &buyer,
        JobRequest {
            kind: JobKind::SpeechSynthesis,
            input: speech_input(SpeechInput::new("covenant compute speaks")),
            model: Some("say-1".into()),
            gpu_class: None,
            min_vram_gb: None,
            min_reputation_bps: None,
            price_micro_usdc: 25_000,
            deadline_ms: 30_000,
        },
    )
    .await
    .expect("the wizard-configured say node serves the speech job");

    let result = parse_speech_output(&outcome.output).expect("output is a speech result");
    assert_eq!(result.model, "say-1");
    assert!(
        !result.audio_base64.is_empty(),
        "the paid-for clip carries synthesized audio"
    );

    let job_id = outcome.receipt.receipt.job_id.to_string();
    let earnings_path = node_home.path().join("earnings.jsonl");
    let mut credited = String::new();
    for _ in 0..100 {
        credited = std::fs::read_to_string(&earnings_path).unwrap_or_default();
        if credited.contains(&job_id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        credited.contains(&job_id),
        "the served speech job is on the earnings ledger"
    );

    let pid = node.id().expect("serving child has a pid");
    assert!(std::process::Command::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap()
        .success());
    let status = tokio::time::timeout(Duration::from_secs(15), node.wait())
        .await
        .expect("drained exit within 15s of SIGTERM")
        .expect("wait on node");
    assert!(status.success(), "a clean drain exits 0, got {status}");
}

/// The streaming relay proven against a real engine: qwen2.5:0.5b generates
/// through the node's chunk forwarder and the coordinator relay to a polling
/// buyer, token by token, and the assembled live feed reconciles with the
/// signed receipt. A mocked backend can prove the plumbing but not that a
/// real generation's stream equals what it receipts. Ignored by default
/// (needs a running Ollama serving qwen2.5:0.5b); run with `--ignored`.
#[tokio::test]
#[ignore = "requires a running ollama at 127.0.0.1:11434 serving qwen2.5:0.5b"]
async fn the_node_binary_streams_a_paid_inference_job_through_real_ollama() {
    let (url, coordinator_pubkey, rail, _coordinator_home) = coordinator().await;
    let node_home = tempfile::tempdir().unwrap();
    let mut node = spawn_node_with(
        &url,
        &coordinator_pubkey,
        node_home.path(),
        &[
            ("COVENANT_COMPUTE_NODE_EXECUTOR", "ollama"),
            ("COVENANT_COMPUTE_NODE_JOB_KINDS", "inference_call"),
            ("COVENANT_COMPUTE_OLLAMA_URL", "http://127.0.0.1:11434"),
            ("COVENANT_COMPUTE_NODE_MODELS", "qwen2.5:0.5b"),
        ],
    );
    wait_registered(&url).await;

    let http = reqwest::Client::new();
    let config = BuyerConfig {
        coordinator_url: url.clone(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    let buyer = LocalIdentity::generate("buyer@test");
    rail.preload(VerifiedDeposit {
        deposit_id: "ollama-stream-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });
    assert!(
        claim_deposit(&http, &config, &buyer, "ollama-stream-deposit")
            .await
            .unwrap()
            .credited
    );

    let mut chunks: Vec<String> = Vec::new();
    let streamed = dispatch_streaming(
        &http,
        &config,
        &buyer,
        JobRequest {
            kind: JobKind::InferenceCall,
            input: vec![Content::text("Count from one to five.")],
            model: Some("qwen2.5:0.5b".into()),
            gpu_class: None,
            min_vram_gb: None,
            min_reputation_bps: None,
            price_micro_usdc: 25_000,
            deadline_ms: 30_000,
        },
        |chunk| chunks.push(chunk.to_string()),
    )
    .await
    .expect("the spawned ollama node streams the job");

    // Real tokens arrived over the relay, and the live feed the buyer saw
    // reconciles byte-for-byte with the output the receipt is signed over.
    let feed: String = chunks.concat();
    assert!(!feed.trim().is_empty(), "the stream relayed real tokens");
    assert!(
        streamed.stream_matched_output,
        "the assembled live feed equals the receipted output"
    );
    let receipt = &streamed.outcome.receipt.receipt;
    assert_eq!(receipt.price_micro_usdc, 25_000);
    assert!(
        receipt.meter.tokens_out.unwrap_or(0) > 0,
        "real completion-token metering, got {:?}",
        receipt.meter.tokens_out
    );

    let pid = node.id().expect("serving child has a pid");
    assert!(std::process::Command::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap()
        .success());
    let status = tokio::time::timeout(Duration::from_secs(15), node.wait())
        .await
        .expect("drained exit within 15s of SIGTERM")
        .expect("wait on node");
    assert!(status.success(), "a clean drain exits 0, got {status}");
}

/// A read command run against a live node's home must open the earnings
/// ledger, the outbox, and the accepted book as pure replays — never the
/// healing `open` a `serve` process uses. Seed each journal with a torn
/// final fragment a live writer would leave mid-append: the healing open
/// truncates all three to empty (and, with settled history, renames them
/// out from under the serve loop's append handle), while the read-only
/// replay leaves every byte in place. Running `earnings` here must leave
/// all three untouched.
#[tokio::test]
async fn an_earnings_read_never_heals_the_serve_loops_journals() {
    let node_home = tempfile::tempdir().unwrap();
    let torn = b"{\"job_id\":\"torn".as_slice();
    for name in ["earnings.jsonl", "outbox.jsonl", "accepted.jsonl"] {
        std::fs::write(node_home.path().join(name), torn).unwrap();
    }

    let report = Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"))
        .env_clear()
        .env("COVENANT_COMPUTE_NODE_HOME", node_home.path())
        .arg("earnings")
        .output()
        .await
        .unwrap();
    assert!(
        report.status.success(),
        "earnings read exits 0 even with torn journals: {}",
        String::from_utf8_lossy(&report.stderr)
    );

    for name in ["earnings.jsonl", "outbox.jsonl", "accepted.jsonl"] {
        assert_eq!(
            std::fs::read(node_home.path().join(name)).unwrap(),
            torn,
            "{name} was healed by a read command — it must stay byte-for-byte intact \
             while the serve loop owns the writes"
        );
    }
}

/// The streaming path out of the real binary: executor deltas ride
/// main.rs's runtime-executor enum, the chunk forwarder, and the
/// coordinator relay from a spawned process to a polling buyer. The
/// receipt cannot see this seam — a binary whose enum wrapper lost its
/// `execute_streaming` forwarding still serves the job green one-shot
/// (that exact regression shipped once) — so the proof is the live
/// feed itself: every delta arrives, in order, chunk boundaries
/// intact, and assembles to the receipted output.
#[tokio::test]
async fn the_node_binary_streams_a_paid_job_through_the_chunk_relay() {
    let (url, coordinator_pubkey, rail, _coordinator_home) = coordinator().await;
    let node_home = tempfile::tempdir().unwrap();
    let mut node = spawn_node(&url, &coordinator_pubkey, node_home.path());
    wait_registered(&url).await;

    let http = reqwest::Client::new();
    let config = BuyerConfig {
        coordinator_url: url.clone(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    let buyer = LocalIdentity::generate("buyer@test");
    rail.preload(VerifiedDeposit {
        deposit_id: "stream-bin-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });
    assert!(
        claim_deposit(&http, &config, &buyer, "stream-bin-deposit")
            .await
            .unwrap()
            .credited
    );

    // Two input blocks are two executor deltas: order and chunk
    // boundaries must survive the whole relay, not just the assembled
    // total.
    let mut chunks: Vec<String> = Vec::new();
    let streamed = dispatch_streaming(
        &http,
        &config,
        &buyer,
        JobRequest {
            kind: JobKind::InferenceCall,
            input: vec![
                Content::text("the first delta, "),
                Content::text("the second delta"),
            ],
            model: None,
            gpu_class: None,
            min_vram_gb: None,
            min_reputation_bps: None,
            price_micro_usdc: 25_000,
            deadline_ms: 30_000,
        },
        |chunk| chunks.push(chunk.to_string()),
    )
    .await
    .expect("the spawned node streams the job");

    assert_eq!(
        chunks,
        vec!["the first delta, ", "the second delta"],
        "every delta arrived as its own chunk, in seq order"
    );
    assert!(
        streamed.stream_matched_output,
        "the assembled live feed equals the receipted output"
    );
    let receipt = &streamed.outcome.receipt.receipt;
    assert_eq!(receipt.price_micro_usdc, 25_000);
    assert_eq!(
        streamed.outcome.output,
        vec![
            Content::text("the first delta, "),
            Content::text("the second delta"),
        ],
        "the verified output is the echoed input, blocks intact"
    );

    // A streamed job must not wedge the drain: the forwarder task ends
    // with the job, so SIGTERM after its conclusion exits as clean as
    // after a one-shot.
    let pid = node.id().expect("serving child has a pid");
    assert!(std::process::Command::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap()
        .success());
    let status = tokio::time::timeout(Duration::from_secs(15), node.wait())
        .await
        .expect("drained exit within 15s of SIGTERM")
        .expect("wait on node");
    assert!(status.success(), "a clean drain exits 0, got {status}");
}

/// One CLI read as the operator runs it: a fresh process against the
/// same home, configured by env exactly like its serve life (so the
/// declared-profile reads in `status` see what the node registered),
/// expected to exit 0 and answer on stdout.
async fn cli(home: &std::path::Path, coordinator_url: &str, args: &[&str]) -> String {
    cli_env(home, coordinator_url, &[], args).await
}

async fn cli_env(
    home: &std::path::Path,
    coordinator_url: &str,
    extra_env: &[(&str, &str)],
    args: &[&str],
) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"));
    cmd.env_clear()
        .env("COVENANT_COMPUTE_NODE_HOME", home)
        .env("COVENANT_COMPUTE_COORDINATOR_URL", coordinator_url)
        .env("COVENANT_COMPUTE_NODE_EXECUTOR", "echo")
        .env("COVENANT_COMPUTE_NODE_JOB_KINDS", "inference_call");
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    let out = cmd.args(args).output().await.unwrap();
    assert!(
        out.status.success(),
        "{args:?} exits 0, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// The operator's standing reads through the real binary: `status`
/// answers "am I winning work" against the live coordinator in both
/// the serving and the drained state — including the market section,
/// which places this node's ask against the live-capacity directory
/// with a cheaper competitor registered — and `bond` prints the exact
/// instructions and memo a stake for this node would need. The
/// hermetic twin of the manual three-state status proof.
#[tokio::test]
async fn the_status_and_bond_subcommands_read_this_nodes_standing_in_both_states() {
    let (url, coordinator_pubkey, rail, _coordinator_home) = coordinator().await;
    let node_home = tempfile::tempdir().unwrap();
    let mut node = spawn_node(&url, &coordinator_pubkey, node_home.path());
    wait_registered(&url).await;

    // One paid job so the money lines have something to report.
    let http = reqwest::Client::new();
    let config = BuyerConfig {
        coordinator_url: url.clone(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    let buyer = LocalIdentity::generate("buyer@test");
    rail.preload(VerifiedDeposit {
        deposit_id: "status-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });
    claim_deposit(&http, &config, &buyer, "status-deposit")
        .await
        .unwrap();
    let outcome = dispatch_and_verify(
        &http,
        &config,
        &buyer,
        JobRequest {
            kind: JobKind::InferenceCall,
            input: vec![Content::text("standing check")],
            model: None,
            gpu_class: None,
            min_vram_gb: None,
            min_reputation_bps: None,
            price_micro_usdc: 25_000,
            deadline_ms: 30_000,
        },
    )
    .await
    .expect("the node serves the job");
    let job_id = outcome.receipt.receipt.job_id.to_string();
    for _ in 0..100 {
        if std::fs::read_to_string(node_home.path().join("earnings.jsonl"))
            .unwrap_or_default()
            .contains(&job_id)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // A cheaper competitor joins the directory (after the paid job, so
    // the price-sorted matcher couldn't have stolen it): a generic node
    // that also serves a named model, at 400 against this node's 1000.
    let rival_home = tempfile::tempdir().unwrap();
    let rival_env = [
        ("COVENANT_COMPUTE_NODE_PRICE_MICRO_USDC", "400"),
        ("COVENANT_COMPUTE_NODE_MODELS", "any,llama"),
    ];
    let _rival = spawn_node_with(&url, &coordinator_pubkey, rival_home.path(), &rival_env);
    wait_registered_count(&url, 2).await;

    // Serving state: registered, live, matchable, money on the books —
    // and the market section places this node's ask against the rival.
    let live = cli(node_home.path(), &url, &["status"]).await;
    let operator_key = live
        .lines()
        .find_map(|l| l.strip_prefix("operator:"))
        .expect("status names the operator")
        .trim()
        .to_string();
    assert!(
        live.contains("advertising:"),
        "status echoes the hardware the node advertises: {live}"
    );
    assert!(
        live.contains("earnings:      1 jobs credited, 25000 micro-USDC earned"),
        "{live}"
    );
    assert!(
        live.contains(&format!("coordinator:   {url} — reachable")),
        "{live}"
    );
    assert!(
        live.contains("directory:     registered, declared online"),
        "{live}"
    );
    assert!(live.contains("matchable:     yes"), "{live}");
    assert!(
        live.contains("market:        2 matchable of 2 registered operator(s) in the directory"),
        "{live}"
    );
    assert!(
        live.contains(
            "  inference_call/any — 2 matchable operator(s), asks 400..1000 micro-USDC \
             (named-model supply of this kind also competes: 1 row(s), asks from 400); your \
             ask 1000 micro-USDC (per_job) sits above the 400 floor — price-sorted matching \
             tries cheaper supply first"
        ),
        "{live}"
    );

    // The rival's own read of the same market: its 400 sets the shared
    // row's floor, and its named-model row is the only offer — with the
    // generic supply that also competes for jobs naming it counted in.
    let rival_view = cli_env(rival_home.path(), &url, &rival_env, &["status"]).await;
    assert!(rival_view.contains("matchable:     yes"), "{rival_view}");
    assert!(
        rival_view.contains(
            "  inference_call/any — 2 matchable operator(s), asks 400..1000 micro-USDC \
             (named-model supply of this kind also competes: 1 row(s), asks from 400); your \
             ask 400 micro-USDC (per_job) sets the row's floor"
        ),
        "{rival_view}"
    );
    assert!(
        rival_view.contains(
            "  inference_call/llama — 1 matchable operator(s), asks 400..400 micro-USDC \
             (+2 generic any-model operator(s) asking from 400); your ask 400 micro-USDC \
             (per_job) is the row's only offer"
        ),
        "{rival_view}"
    );

    // Drain, then ask again: the same read must now say NO and name
    // the clean shutdown as the reason, while the home's money lines
    // outlive the process.
    let pid = node.id().expect("serving child has a pid");
    assert!(std::process::Command::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap()
        .success());
    let status = tokio::time::timeout(Duration::from_secs(15), node.wait())
        .await
        .expect("drained exit within 15s of SIGTERM")
        .expect("wait on node");
    assert!(status.success());

    let drained = cli(node_home.path(), &url, &["status"]).await;
    assert!(drained.contains("matchable:     NO"), "{drained}");
    assert!(drained.contains("declared itself offline"), "{drained}");
    assert!(
        drained.contains("earnings:      1 jobs credited"),
        "{drained}"
    );

    // The drained node still reads the market it would rejoin: the
    // rival's row stands alone, and the wording flips to conditional.
    assert!(
        drained.contains("market:        1 matchable of 2 registered operator(s) in the directory"),
        "{drained}"
    );
    assert!(
        drained.contains(
            "  inference_call/any — 1 matchable operator(s), asks 400..400 micro-USDC \
             (named-model supply of this kind also competes: 1 row(s), asks from 400); your \
             ask 1000 micro-USDC (per_job) would sit above the 400 floor"
        ),
        "{drained}"
    );

    // A pairing nobody serves: the market read names the vacancy.
    let vacant = cli_env(
        node_home.path(),
        &url,
        &[("COVENANT_COMPUTE_NODE_MODELS", "mistral")],
        &["status"],
    )
    .await;
    assert!(
        vacant.contains(
            "  inference_call/mistral — no matchable supply serves this pairing right now; \
             your ask 1000 micro-USDC (per_job) would be the only offer"
        ),
        "{vacant}"
    );

    // `bond` with nothing posted: the zero book plus the exact staking
    // instructions, memo included, for THIS node's key.
    let bond = cli(node_home.path(), &url, &["bond"]).await;
    assert!(bond.contains("posted:            0 micro-USDC"), "{bond}");
    assert!(bond.contains("nothing posted yet"), "{bond}");
    assert!(
        bond.contains(&format!(
            "memo for this node: {}{operator_key}",
            covenant_compute_protocol::BOND_MEMO_PREFIX
        )),
        "{bond}"
    );
}

/// The read subcommands answer as machine JSON under `--json` — the shape
/// a monitor or a scheduled health check parses, and the twin of the
/// buyer CLI's `--json`. Each is spawned as the real binary against a
/// live coordinator and its stdout parsed back.
#[tokio::test]
async fn the_read_subcommands_answer_as_json_under_the_flag() {
    let (url, _pubkey, _rail, _coord_home) = coordinator().await;
    let node_home = tempfile::tempdir().unwrap();

    // earnings --json: a fresh node has an empty, well-formed ledger.
    let earnings = cli(node_home.path(), &url, &["earnings", "--json"]).await;
    let earnings: serde_json::Value =
        serde_json::from_str(&earnings).expect("earnings --json is valid JSON");
    assert_eq!(earnings["jobs_credited"], 0);
    assert_eq!(earnings["unpaid_micro_usdc"], 0);
    assert!(
        earnings["entries"].is_array(),
        "entries is an array: {earnings}"
    );

    // status --json: the local facts plus the live coordinator read.
    let status = cli(node_home.path(), &url, &["status", "--json"]).await;
    let status: serde_json::Value =
        serde_json::from_str(&status).expect("status --json is valid JSON");
    assert!(
        status["operator_pubkey_b58"]
            .as_str()
            .is_some_and(|k| !k.is_empty()),
        "status names the operator: {status}"
    );
    assert_eq!(status["coordinator"]["reachable"], true);
    assert!(
        status["hardware"].is_object(),
        "hardware is structured: {status}"
    );

    // bond --json: the stake feed, zero on a node that never posted.
    let bond = cli(node_home.path(), &url, &["bond", "--json"]).await;
    let bond: serde_json::Value = serde_json::from_str(&bond).expect("bond --json is valid JSON");
    assert!(
        bond["operator_pubkey_b58"]
            .as_str()
            .is_some_and(|k| !k.is_empty()),
        "bond names the operator: {bond}"
    );
    assert_eq!(bond["bond"]["status"]["posted_micro_usdc"], 0);

    // the flag can sit before the subcommand too.
    let placed = cli(node_home.path(), &url, &["--json", "earnings"]).await;
    serde_json::from_str::<serde_json::Value>(&placed)
        .expect("--json before the subcommand parses");
}

/// `status --json` stays valid JSON on the real setup path — a home with
/// a `node.env` whose values are applied on this run. The applied-defaults
/// line (and any startup warning) belongs on stderr; stdout is the
/// document a monitor parses. Regression: the two shared stdout, so a
/// node configured through `setup` (not the environment) emitted an INFO
/// line ahead of the JSON and broke every `--json` consumer.
#[tokio::test]
async fn status_json_stays_valid_json_while_applying_node_env_defaults() {
    let node_home = tempfile::tempdir().unwrap();
    // A knob the cli harness never sets, so apply_node_env_defaults finds
    // it unset in the environment and logs "applying node.env defaults".
    std::fs::write(
        node_home.path().join("node.env"),
        "COVENANT_COMPUTE_NODE_PRICE_MICRO_USDC=13579\n",
    )
    .unwrap();

    // No coordinator: status degrades to reachable:false and still exits 0.
    let stdout = cli(
        node_home.path(),
        "http://127.0.0.1:1",
        &["status", "--json"],
    )
    .await;
    let doc: serde_json::Value =
        serde_json::from_str(&stdout).expect("status --json stdout is valid JSON");
    assert_eq!(doc["home"], node_home.path().display().to_string());
}

/// `bond` needs the coordinator — a node's stake lives there — so when it
/// is down the operator gets one clean line naming the coordinator, the
/// way `status` already does, not a raw reqwest cause chain. Regression:
/// it surfaced "tcp connect error / Connection refused (os error 61)".
#[tokio::test]
async fn bond_names_an_unreachable_coordinator_without_a_reqwest_chain() {
    let node_home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"))
        .env_clear()
        .env("COVENANT_COMPUTE_NODE_HOME", node_home.path())
        // A port nothing listens on: the connection is refused at once.
        .env("COVENANT_COMPUTE_COORDINATOR_URL", "http://127.0.0.1:1")
        .arg("bond")
        .output()
        .await
        .unwrap();
    assert!(
        !out.status.success(),
        "bond exits nonzero when the coordinator is down"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("coordinator at http://127.0.0.1:1"),
        "names the coordinator plainly: {stderr}"
    );
    assert!(
        !stderr.contains("os error") && !stderr.contains("Caused by"),
        "no raw reqwest cause chain leaks through: {stderr}"
    );
}

/// B5's refusal half: a node that cannot serve a claim it declared must
/// refuse to register, not surface the failure later as a stranger's
/// refunded job. An openai-compat profile pinned to a model whose backend
/// is unreachable fails its benchmark; the process exits before it ever
/// reaches registration. The happy path is covered above — this is the
/// half that keeps a broken node out of the directory.
#[tokio::test]
async fn a_node_that_cannot_serve_its_claims_refuses_to_register() {
    let node_home = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"));
    cmd.env_clear()
        .env("COVENANT_COMPUTE_NODE_HOME", node_home.path())
        // The benchmark runs before any coordinator contact, so a dead
        // coordinator is never reached — the node fails first at the probe.
        .env("COVENANT_COMPUTE_COORDINATOR_URL", "http://127.0.0.1:1")
        .env(
            "COVENANT_COMPUTE_COORDINATOR_PUBKEY",
            test_coordinator_pubkey(),
        )
        .env("COVENANT_COMPUTE_PAYOUT_ADDRESS", test_payout_address())
        .env("COVENANT_COMPUTE_NODE_EXECUTOR", "openai-compat")
        // A port nothing listens on: the benchmark's generate call is
        // refused at once, so the probe fails deterministically.
        .env("COVENANT_COMPUTE_OPENAI_URL", "http://127.0.0.1:1")
        // Pinned explicitly so model discovery is skipped and the run
        // reaches the benchmark with a concrete claim to prove.
        .env("COVENANT_COMPUTE_NODE_MODELS", "phantom-model")
        .env("COVENANT_COMPUTE_NODE_JOB_KINDS", "inference_call")
        .env("COVENANT_COMPUTE_BENCHMARK_TIMEOUT_SECS", "5")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(60), cmd.output())
        .await
        .expect("a failed benchmark exits the node instead of serving")
        .expect("spawn covenant-compute-node");
    assert!(
        !out.status.success(),
        "a node that fails its benchmark must exit nonzero"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("refusing to register a profile this node cannot serve"),
        "the exit must name the benchmark refusal, got: {stderr}"
    );
}

/// Spawns the node binary against the real local Ollama with a
/// deliberately mismatched `(job_kind, model)` claim, and returns the
/// stderr it exits with. The coordinator URL points at a dead port on
/// purpose: the benchmark runs before any coordinator contact, so the
/// probe is what fails, and the process never registers. Both models
/// pinned by the callers exist in Ollama, so this reaches the benchmark
/// rather than tripping a model-presence check — the point is a backend
/// that is up and holds the model yet cannot serve the declared kind.
async fn ollama_benchmark_refusal_stderr(job_kinds: &str, model: &str) -> String {
    let node_home = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"));
    cmd.env_clear()
        .env("COVENANT_COMPUTE_NODE_HOME", node_home.path())
        .env("COVENANT_COMPUTE_COORDINATOR_URL", "http://127.0.0.1:1")
        .env(
            "COVENANT_COMPUTE_COORDINATOR_PUBKEY",
            test_coordinator_pubkey(),
        )
        .env("COVENANT_COMPUTE_PAYOUT_ADDRESS", test_payout_address())
        .env("COVENANT_COMPUTE_NODE_EXECUTOR", "ollama")
        .env("COVENANT_COMPUTE_OLLAMA_URL", "http://127.0.0.1:11434")
        .env("COVENANT_COMPUTE_NODE_MODELS", model)
        .env("COVENANT_COMPUTE_NODE_JOB_KINDS", job_kinds)
        .env("COVENANT_COMPUTE_BENCHMARK_TIMEOUT_SECS", "30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(90), cmd.output())
        .await
        .expect("a failed benchmark exits the node instead of hanging")
        .expect("spawn covenant-compute-node");
    assert!(
        !out.status.success(),
        "a node that fails its benchmark must exit nonzero"
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// B5's refusal half against a real, healthy backend that simply cannot
/// serve the claim — the case the dead-backend probe above can't reach.
/// A node declaring `embedding` while pinned to a chat-only model
/// (`qwen2.5:0.5b`) drives a real embed probe into Ollama, which answers
/// `501 does not support embeddings`; the benchmark reads that as a
/// failed claim and the node exits before registering. Proves the gate
/// reads a live backend's own "can't do that", not merely a refused
/// connection, and pins the exact mismatch that a mock executor returns
/// clean vectors for. Run with `--ignored`.
#[tokio::test]
#[ignore = "requires a running ollama at 127.0.0.1:11434 serving qwen2.5:0.5b"]
async fn a_node_claiming_embeddings_on_a_chat_only_model_refuses_to_register() {
    let stderr = ollama_benchmark_refusal_stderr("embedding", "qwen2.5:0.5b").await;
    assert!(
        stderr.contains("refusing to register a profile this node cannot serve"),
        "the exit must name the benchmark refusal, got: {stderr}"
    );
    assert!(
        stderr.contains("qwen2.5:0.5b"),
        "the failing model must be named in the refusal, got: {stderr}"
    );
    assert!(
        stderr.contains("does not support embeddings"),
        "the live backend's own reason must be surfaced, got: {stderr}"
    );
}

/// The symmetric live refusal: a node declaring `inference_call` while
/// pinned to an embedding-only model (`nomic-embed-text`). Ollama fails
/// the generation probe with `400 does not support generate`, so the
/// benchmark refuses the claim and the node never registers. Together
/// with the embedding case above, this proves the register-time gate
/// fails closed on a backend that is reachable and holds the model but
/// cannot perform the declared kind. Run with `--ignored`.
#[tokio::test]
#[ignore = "requires a running ollama at 127.0.0.1:11434 serving nomic-embed-text"]
async fn a_node_claiming_inference_on_an_embedding_only_model_refuses_to_register() {
    let stderr = ollama_benchmark_refusal_stderr("inference_call", "nomic-embed-text").await;
    assert!(
        stderr.contains("refusing to register a profile this node cannot serve"),
        "the exit must name the benchmark refusal, got: {stderr}"
    );
    assert!(
        stderr.contains("nomic-embed-text"),
        "the failing model must be named in the refusal, got: {stderr}"
    );
    assert!(
        stderr.contains("does not support"),
        "the live backend's own reason must be surfaced, got: {stderr}"
    );
}

/// A node running as a managed service writes its running log to the
/// rotating directory the agent points it at — the launchd path, where
/// stdout is a single file launchd never rotates. Proven through the real
/// binary: LOG_DIR set, stdout null, and the boot lines still land in a
/// dated file under that directory.
#[tokio::test]
async fn a_serviced_node_writes_its_log_to_the_rotating_directory() {
    let (url, coordinator_pubkey, _rail, _coordinator_home) = coordinator().await;
    let node_home = tempfile::tempdir().unwrap();
    let log_dir = tempfile::tempdir().unwrap();
    let mut node = spawn_node_with(
        &url,
        &coordinator_pubkey,
        node_home.path(),
        &[(
            "COVENANT_COMPUTE_NODE_LOG_DIR",
            log_dir.path().to_str().unwrap(),
        )],
    );
    wait_registered(&url).await;

    // Registration is well past the identity line, so a dated log file
    // with that line must already exist in the directory (stdout is null).
    let mut logged = false;
    for _ in 0..50 {
        let dated = std::fs::read_dir(log_dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .find(|n| n.starts_with("node.") && n.ends_with(".log"));
        if let Some(name) = dated {
            if std::fs::read_to_string(log_dir.path().join(name))
                .unwrap()
                .contains("operator identity ready")
            {
                logged = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        logged,
        "the serviced node's log belongs in the rotating dir"
    );

    node.start_kill().ok();
    let _ = node.wait().await;
}

/// A container node that advertises a GPU but passes no device is told,
/// in `status`, that those jobs run CPU-only — and the line is gone once
/// a device is configured. `status` is where an operator debugging
/// missed GPU work looks; the boot warning scrolls past under a service
/// manager. Hermetic: the coherence line prints before the coordinator
/// check, so no coordinator is needed.
#[tokio::test]
async fn status_flags_a_gpu_advertised_but_not_passed_into_the_container() {
    let node_home = tempfile::tempdir().unwrap();
    let gpu_env: &[(&str, &str)] = &[
        ("COVENANT_COMPUTE_NODE_EXECUTOR", "container"),
        (
            "COVENANT_COMPUTE_NODE_HARDWARE",
            "consumer:NVIDIA GeForce RTX 4090",
        ),
        ("COVENANT_COMPUTE_NODE_VRAM_GB", "24"),
    ];

    let advertised = cli_env(node_home.path(), "http://127.0.0.1:1", gpu_env, &["status"]).await;
    assert!(
        advertised.contains("advertising:   NVIDIA GeForce RTX 4090"),
        "status echoes the advertised GPU: {advertised}"
    );
    assert!(
        advertised.contains("the container passes no device"),
        "an undelivered GPU must be flagged in status: {advertised}"
    );

    // Pass the device and the warning is gone.
    let mut coherent_env = gpu_env.to_vec();
    coherent_env.push(("COVENANT_COMPUTE_NODE_CONTAINER_GPUS", "all"));
    let coherent = cli_env(
        node_home.path(),
        "http://127.0.0.1:1",
        &coherent_env,
        &["status"],
    )
    .await;
    assert!(
        !coherent.contains("the container passes no device"),
        "no warning once the GPU is passed through: {coherent}"
    );
}

/// One node life that serves a single paid job and drains: spawn,
/// register, purchase, wait until the home's earnings ledger carries
/// `marker`, SIGTERM. Returns the job id. The marker lets callers wait
/// for the credit itself or for the payout-reconcile flip.
async fn serve_one_paid_job_until(
    url: &str,
    coordinator_pubkey: &str,
    rail: &MockRail,
    node_home: &std::path::Path,
    deposit_id: &str,
    marker: &str,
) -> String {
    let mut node = spawn_node(url, coordinator_pubkey, node_home);
    wait_registered(url).await;
    let http = reqwest::Client::new();
    let config = BuyerConfig {
        coordinator_url: url.to_string(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    let buyer = LocalIdentity::generate("buyer@test");
    rail.preload(VerifiedDeposit {
        deposit_id: deposit_id.into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });
    claim_deposit(&http, &config, &buyer, deposit_id)
        .await
        .unwrap();
    let outcome = dispatch_and_verify(
        &http,
        &config,
        &buyer,
        JobRequest {
            kind: JobKind::InferenceCall,
            input: vec![Content::text("a paid question")],
            model: None,
            gpu_class: None,
            min_vram_gb: None,
            min_reputation_bps: None,
            price_micro_usdc: 25_000,
            deadline_ms: 30_000,
        },
    )
    .await
    .expect("the node serves the job");
    let job_id = outcome.receipt.receipt.job_id.to_string();

    let ledger = node_home.join("earnings.jsonl");
    let mut seen = String::new();
    for _ in 0..150 {
        seen = std::fs::read_to_string(&ledger).unwrap_or_default();
        if seen.contains(marker) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(seen.contains(marker), "ledger never carried {marker:?}");

    let pid = node.id().expect("serving child has a pid");
    assert!(std::process::Command::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap()
        .success());
    let status = tokio::time::timeout(Duration::from_secs(15), node.wait())
        .await
        .expect("drained exit within 15s of SIGTERM")
        .expect("wait on node");
    assert!(status.success());
    job_id
}

/// `earnings verify`'s green half through the real binary: rows paid
/// by a backend that submitted nothing on-chain are reported as
/// offchain-only — honestly, loudly, and with exit 0, because nothing
/// the books claim is contradicted.
#[tokio::test]
async fn earnings_verify_reports_offchain_paid_rows_and_exits_clean() {
    let (url, coordinator_pubkey, rail, _coordinator_home) = coordinator().await;
    let node_home = tempfile::tempdir().unwrap();
    let job_id = serve_one_paid_job_until(
        &url,
        &coordinator_pubkey,
        &rail,
        node_home.path(),
        "verify-offchain-deposit",
        "\"status\":\"paid\"",
    )
    .await;

    // The RPC endpoint is unroutable on purpose: offchain rows carry
    // no transaction, so a verify that touches the network at all has
    // misclassified them.
    let out = Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"))
        .env_clear()
        .env("COVENANT_COMPUTE_NODE_HOME", node_home.path())
        .env("COVENANT_COMPUTE_NODE_RPC_URL", "http://127.0.0.1:1")
        .args(["earnings", "verify"])
        .output()
        .await
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "offchain-only books exit 0, said: {stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "{job_id}  offchain record only — the payout backend submitted no transaction"
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains("0 verified on-chain, 0 pending, 1 offchain-only, 0 unverifiable, 0 not yet paid, 0 contradicted, 0 unreachable"),
        "{stdout}"
    );
}

/// `earnings verify`'s red half: a paid row that names a transaction
/// the operator's own RPC cannot answer for must fail the run — the
/// cron contract is silence == every paid row proven, so an
/// unreachable proof is a nonzero exit, not a warning.
#[tokio::test]
async fn earnings_verify_fails_the_run_when_a_paid_rows_transaction_cannot_be_fetched() {
    let (url, coordinator_pubkey, rail, _coordinator_home) =
        coordinator_with_payout(Arc::new(StampingPayout)).await;
    let node_home = tempfile::tempdir().unwrap();
    let job_id = serve_one_paid_job_until(
        &url,
        &coordinator_pubkey,
        &rail,
        node_home.path(),
        "verify-stamped-deposit",
        "\"paid_tx_signature\":\"stamped-",
    )
    .await;

    let out = Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"))
        .env_clear()
        .env("COVENANT_COMPUTE_NODE_HOME", node_home.path())
        .env("COVENANT_COMPUTE_NODE_RPC_URL", "http://127.0.0.1:1")
        .args(["earnings", "verify"])
        .output()
        .await
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "an unprovable paid row must fail the run, said: {stdout}"
    );
    assert!(
        stdout.contains(&format!("{job_id}  unreachable")),
        "{stdout}"
    );
    assert!(
        stdout.contains("0 verified on-chain, 0 pending, 0 offchain-only, 0 unverifiable, 0 not yet paid, 0 contradicted, 1 unreachable"),
        "{stdout}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("1 paid rows did not verify"), "{stderr}");
}

/// `earnings verify` reads back through the operator's own RPC. A
/// schemeless COVENANT_COMPUTE_NODE_RPC_URL would fail deep in reqwest;
/// it must be refused up front with a message that names the fix, before
/// the ledger is even opened — so no paid rows or coordinator are needed.
#[tokio::test]
async fn earnings_verify_refuses_a_schemeless_rpc_url() {
    let node_home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"))
        .env_clear()
        .env("COVENANT_COMPUTE_NODE_HOME", node_home.path())
        .env("COVENANT_COMPUTE_NODE_RPC_URL", "localhost:8899")
        .args(["earnings", "verify"])
        .output()
        .await
        .unwrap();
    assert!(
        !out.status.success(),
        "a schemeless rpc_url must fail the run"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("http://"),
        "the refusal must name the scheme fix: {stderr}"
    );
}

/// The bond book driven end to end through the CLI: claim a preloaded
/// on-chain post, re-claim it idempotently, take stake back out with
/// `bond unbond`, and watch every read change. No serve loop involved
/// — exactly the lifecycle of an operator staking before their first
/// job.
#[tokio::test]
async fn the_bond_subcommands_claim_and_unbond_stake_through_the_real_binary() {
    let (url, _coordinator_pubkey, rail, _coordinator_home) = coordinator().await;
    let node_home = tempfile::tempdir().unwrap();
    // Created up front so the rail can name the key the CLI will load.
    let identity =
        LocalIdentity::load_or_create(&node_home.path().join("identity.json"), "operator@compute")
            .unwrap();
    let operator_key = identity.agent_id().pubkey_base58();
    rail.preload_bond(VerifiedBond {
        bond_id: "bond-tx-signature".into(),
        operator_pubkey_b58: operator_key.clone(),
        amount_micro_usdc: 50_000,
    });

    let claimed = cli(
        node_home.path(),
        &url,
        &["bond", "claim", "bond-tx-signature"],
    )
    .await;
    assert!(
        claimed.contains("credited 50000 micro-USDC")
            && claimed.contains("posted total 50000 micro-USDC"),
        "both amounts carry their unit and dollar value: {claimed}"
    );
    let reclaimed = cli(
        node_home.path(),
        &url,
        &["bond", "claim", "bond-tx-signature"],
    )
    .await;
    assert!(
        reclaimed.contains("already credited — posted total 50000 micro-USDC"),
        "{reclaimed}"
    );

    let posted = cli(node_home.path(), &url, &["bond"]).await;
    assert!(
        posted.contains("posted:            50000 micro-USDC"),
        "{posted}"
    );
    assert!(
        posted.contains("committed:         50000 micro-USDC"),
        "{posted}"
    );

    let unbonded = cli(
        node_home.path(),
        &url,
        &["bond", "unbond", "20000", &operator_key],
    )
    .await;
    assert!(
        unbonded.contains("registered: 20000 micro-USDC")
            && unbonded.contains(&format!("to {operator_key}")),
        "the amount carries its unit and the recipient renders plain, not as a quoted JSON \
         string: {unbonded}"
    );
    assert!(unbonded.contains("matures at epoch-ms"), "{unbonded}");

    // The requested stake leaves the matcher's view immediately but
    // stays slashable until the refund actually pushes.
    let after = cli(node_home.path(), &url, &["bond"]).await;
    assert!(
        after.contains("unbonding:         20000 micro-USDC"),
        "{after}"
    );
    assert!(
        after.contains("committed:         30000 micro-USDC"),
        "{after}"
    );
    assert!(
        after.contains("at stake:          50000 micro-USDC"),
        "{after}"
    );
    assert!(after.contains("maturing until epoch-ms"), "{after}");

    // Taking out more than is committed refuses upstream; the CLI
    // carries the refusal into its exit code.
    let out = Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"))
        .env_clear()
        .env("COVENANT_COMPUTE_NODE_HOME", node_home.path())
        .env("COVENANT_COMPUTE_COORDINATOR_URL", &url)
        .args(["bond", "unbond", "1000000", &operator_key])
        .output()
        .await
        .unwrap();
    assert!(!out.status.success(), "an overdrawn unbond exits nonzero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unbond refused"), "{stderr}");
}

/// B3's onboarding seam end to end: the wizard runs as a real process
/// with its three anchors answered over piped stdin, and the home it
/// writes is the whole boot contract — a second life of the binary
/// gets nothing but the home path and must register and serve a paid
/// job from `node.env` alone, under the identity the wizard minted.
#[tokio::test]
async fn the_setup_wizard_writes_a_home_the_binary_boots_and_serves_from() {
    let (url, coordinator_pubkey, rail, _coordinator_home) = coordinator().await;
    let node_home = tempfile::tempdir().unwrap();
    let payout = LocalIdentity::generate("wallet@test")
        .agent_id()
        .pubkey_base58();

    let mut setup = Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"))
        .env_clear()
        .env("COVENANT_COMPUTE_NODE_HOME", node_home.path())
        .args(["setup", "--executor", "subprocess"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn setup wizard");
    let mut answers = setup.stdin.take().expect("wizard stdin");
    answers
        .write_all(format!("{url}\n{coordinator_pubkey}\n{payout}\n").as_bytes())
        .await
        .unwrap();
    drop(answers);
    let out = tokio::time::timeout(Duration::from_secs(20), setup.wait_with_output())
        .await
        .expect("the wizard finishes within 20s")
        .expect("wait on wizard");
    let transcript = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "setup exits 0, said: {transcript}");
    let wizard_pubkey = transcript
        .lines()
        .find_map(|l| l.strip_prefix("operator identity: "))
        .expect("the wizard names the minted identity")
        .to_string();
    assert!(
        transcript.contains("disclosed marketplace fee: 0 bps"),
        "the reachability probe reported the live coordinator's fee: {transcript}"
    );
    assert!(transcript.contains("setup complete"), "{transcript}");

    let env_file = std::fs::read_to_string(node_home.path().join("node.env")).unwrap();
    for line in [
        format!("COVENANT_COMPUTE_COORDINATOR_URL={url}"),
        format!("COVENANT_COMPUTE_COORDINATOR_PUBKEY={coordinator_pubkey}"),
        format!("COVENANT_COMPUTE_PAYOUT_ADDRESS={payout}"),
        "COVENANT_COMPUTE_NODE_EXECUTOR=subprocess".to_string(),
    ] {
        assert!(env_file.contains(&line), "node.env carries {line}");
    }

    // Second life: nothing but the home. Every anchor comes off
    // node.env — the one env knob set here is test cadence, not
    // configuration the wizard owns.
    let mut node = Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"))
        .env_clear()
        .env("COVENANT_COMPUTE_NODE_HOME", node_home.path())
        .env("COVENANT_COMPUTE_NODE_HEARTBEAT_SECS", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("boot from node.env");
    wait_registered(&url).await;

    // The subprocess node runs a real paid command, receipted under
    // the exact identity the wizard minted.
    let http = reqwest::Client::new();
    let config = BuyerConfig {
        coordinator_url: url.clone(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    let buyer = LocalIdentity::generate("buyer@test");
    rail.preload(VerifiedDeposit {
        deposit_id: "setup-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });
    assert!(
        claim_deposit(&http, &config, &buyer, "setup-deposit")
            .await
            .unwrap()
            .credited
    );
    let outcome = dispatch_and_verify(
        &http,
        &config,
        &buyer,
        JobRequest {
            kind: JobKind::BatchJob,
            input: vec![Content::text("printf %s setup-proven")],
            model: None,
            gpu_class: None,
            min_vram_gb: None,
            min_reputation_bps: None,
            price_micro_usdc: 25_000,
            deadline_ms: 30_000,
        },
    )
    .await
    .expect("the wizard-configured node serves the job");
    match &outcome.output[0] {
        Content::Text { text } => assert_eq!(text, "setup-proven"),
        other => panic!("command stdout expected, got {other:?}"),
    }
    assert_eq!(
        outcome.receipt.receipt.operator.pubkey_base58(),
        wizard_pubkey,
        "the serving identity is the one the wizard minted"
    );

    // The composed home drains as clean as an env-configured one.
    let pid = node.id().expect("serving child has a pid");
    assert!(std::process::Command::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap()
        .success());
    let status = tokio::time::timeout(Duration::from_secs(15), node.wait())
        .await
        .expect("drained exit within 15s of SIGTERM")
        .expect("wait on node");
    assert!(status.success(), "a clean drain exits 0, got {status}");
}

/// An operator who points a model-serving node at a dead backend must
/// get a hard boot failure, not a registered profile the node cannot
/// serve — the fail-closed half of the benchmark gate, before any
/// coordinator traffic.
#[tokio::test]
async fn a_model_serving_boot_with_a_dead_backend_fails_closed_before_registering() {
    let node_home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"))
        .env_clear()
        .env("COVENANT_COMPUTE_NODE_HOME", node_home.path())
        // Never contacted: the boot dies at model discovery first.
        .env("COVENANT_COMPUTE_COORDINATOR_URL", "http://127.0.0.1:1")
        .env(
            "COVENANT_COMPUTE_COORDINATOR_PUBKEY",
            test_coordinator_pubkey(),
        )
        .env("COVENANT_COMPUTE_PAYOUT_ADDRESS", test_payout_address())
        .env("COVENANT_COMPUTE_NODE_EXECUTOR", "ollama")
        .env("COVENANT_COMPUTE_OLLAMA_URL", "http://127.0.0.1:1")
        .output()
        .await
        .unwrap();
    assert!(!out.status.success(), "a dead backend must fail the boot");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cannot discover models"), "got: {stderr}");
}

/// A payout address that can't receive money fails the boot before any
/// work is taken — not at the first payout push, where the earnings
/// for served jobs would already be stuck behind a typo the retry
/// sweep can't fix. The wizard validates on entry; this is the
/// hand-written node.env path.
#[tokio::test]
async fn a_malformed_payout_address_fails_the_boot_before_any_work() {
    let node_home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"))
        .env_clear()
        .env("COVENANT_COMPUTE_NODE_HOME", node_home.path())
        // Never contacted: the boot refuses the payout address first.
        .env("COVENANT_COMPUTE_COORDINATOR_URL", "http://127.0.0.1:1")
        .env(
            "COVENANT_COMPUTE_COORDINATOR_PUBKEY",
            test_coordinator_pubkey(),
        )
        .env("COVENANT_COMPUTE_PAYOUT_ADDRESS", "my-wallet-address")
        .env("COVENANT_COMPUTE_NODE_EXECUTOR", "echo")
        .output()
        .await
        .unwrap();
    assert!(
        !out.status.success(),
        "an unpayable address must fail the boot"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("COVENANT_COMPUTE_PAYOUT_ADDRESS") && stderr.contains("base58"),
        "the refusal names the variable and the defect: {stderr}"
    );
}

/// A malformed coordinator pubkey fails the boot before any work is
/// taken. It is the pinned anchor every escrow hold verifies against, so
/// a typo would otherwise admit nothing — the node would win matches and
/// fault its own standing rejecting every one. The wizard validates on
/// entry; this is the hand-written node.env path.
#[tokio::test]
async fn a_malformed_coordinator_pubkey_fails_the_boot_before_any_work() {
    let node_home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"))
        .env_clear()
        .env("COVENANT_COMPUTE_NODE_HOME", node_home.path())
        // Never contacted: the boot refuses the pinned key first.
        .env("COVENANT_COMPUTE_COORDINATOR_URL", "http://127.0.0.1:1")
        .env("COVENANT_COMPUTE_COORDINATOR_PUBKEY", "not-a-real-key")
        .env("COVENANT_COMPUTE_PAYOUT_ADDRESS", test_payout_address())
        .env("COVENANT_COMPUTE_NODE_EXECUTOR", "echo")
        .output()
        .await
        .unwrap();
    assert!(
        !out.status.success(),
        "a malformed coordinator key must fail the boot"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("COVENANT_COMPUTE_COORDINATOR_PUBKEY") && stderr.contains("base58"),
        "the refusal names the variable and the defect: {stderr}"
    );
}

/// A zero price fails the boot rather than registering a node that wins
/// every match and serves it for free — an unpriced buyer call resolves
/// its offer to the cheapest ask, so a 0 ask is served for nothing. The
/// wizard refuses 0 on entry; this is the hand-written node.env path.
#[tokio::test]
async fn a_zero_price_fails_the_boot_rather_than_serving_for_free() {
    let node_home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"))
        .env_clear()
        .env("COVENANT_COMPUTE_NODE_HOME", node_home.path())
        // Never contacted: the boot refuses the free price first. MODELS
        // is set so discovery doesn't precede the price guard.
        .env("COVENANT_COMPUTE_COORDINATOR_URL", "http://127.0.0.1:1")
        .env(
            "COVENANT_COMPUTE_COORDINATOR_PUBKEY",
            test_coordinator_pubkey(),
        )
        .env("COVENANT_COMPUTE_PAYOUT_ADDRESS", test_payout_address())
        .env("COVENANT_COMPUTE_NODE_EXECUTOR", "echo")
        .env("COVENANT_COMPUTE_NODE_MODELS", "any")
        .env("COVENANT_COMPUTE_NODE_PRICE_MICRO_USDC", "0")
        .output()
        .await
        .unwrap();
    assert!(!out.status.success(), "a zero price must fail the boot");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("COVENANT_COMPUTE_NODE_PRICE_MICRO_USDC") && stderr.contains("free"),
        "the refusal names the variable and the consequence: {stderr}"
    );
}

/// A hand-edited node.env with a non-numeric max-fee-bps refuses cleanly
/// instead of panicking — the same class of onboarding cliff as an
/// unpayable address, caught with a message that names the fix rather
/// than a raw Rust backtrace.
#[tokio::test]
async fn a_non_numeric_max_fee_bps_refuses_cleanly_without_panicking() {
    let node_home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"))
        .env_clear()
        .env("COVENANT_COMPUTE_NODE_HOME", node_home.path())
        .env("COVENANT_COMPUTE_COORDINATOR_URL", "http://127.0.0.1:1")
        .env("COVENANT_COMPUTE_COORDINATOR_PUBKEY", test_payout_address())
        .env("COVENANT_COMPUTE_PAYOUT_ADDRESS", test_payout_address())
        .env("COVENANT_COMPUTE_NODE_EXECUTOR", "echo")
        .env("COVENANT_COMPUTE_NODE_MAX_FEE_BPS", "2.5%")
        .output()
        .await
        .unwrap();
    assert!(
        !out.status.success(),
        "a non-numeric max-fee-bps must fail the boot"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("COVENANT_COMPUTE_NODE_MAX_FEE_BPS") && stderr.contains("basis points"),
        "the refusal names the variable and the defect: {stderr}"
    );
    assert!(
        !stderr.to_lowercase().contains("panic"),
        "a clean refusal, not a panic: {stderr}"
    );
}

/// A node must not advertise a capability its backend can't serve. The
/// subprocess executor runs shell commands; told to serve inference, it
/// would shell-run the prompt and bill the buyer for the garbage. Boot
/// refuses the mismatch before registering, naming the servable set.
#[tokio::test]
async fn an_executor_that_cannot_serve_an_advertised_kind_fails_the_boot() {
    let node_home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"))
        .env_clear()
        .env("COVENANT_COMPUTE_NODE_HOME", node_home.path())
        // Never contacted: the boot refuses the mismatch first.
        .env("COVENANT_COMPUTE_COORDINATOR_URL", "http://127.0.0.1:1")
        .env("COVENANT_COMPUTE_COORDINATOR_PUBKEY", test_payout_address())
        .env("COVENANT_COMPUTE_PAYOUT_ADDRESS", test_payout_address())
        .env("COVENANT_COMPUTE_NODE_EXECUTOR", "subprocess")
        .env("COVENANT_COMPUTE_NODE_JOB_KINDS", "inference_call")
        .output()
        .await
        .unwrap();
    assert!(
        !out.status.success(),
        "a backend that can't serve the advertised kind must fail the boot"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("inference_call") && stderr.contains("batch_job"),
        "the refusal names the unservable kind and the servable set: {stderr}"
    );
    assert!(
        !stderr.to_lowercase().contains("panic"),
        "a clean refusal, not a panic: {stderr}"
    );
}

/// `status` is where an operator checks their setup before serving, so it
/// flags a JOB_KINDS the executor can't serve — the same mismatch the
/// boot refuses — instead of leaving them to a failed serve to find out.
#[tokio::test]
async fn status_flags_a_job_kind_the_executor_cannot_serve() {
    let node_home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"))
        .env_clear()
        .env("COVENANT_COMPUTE_NODE_HOME", node_home.path())
        // No coordinator: status prints local diagnostics and exits clean.
        .env("COVENANT_COMPUTE_NODE_EXECUTOR", "subprocess")
        .env("COVENANT_COMPUTE_NODE_JOB_KINDS", "inference_call")
        .arg("status")
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "status runs without a coordinator: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("inference_call")
            && stdout.contains("batch_job")
            && stdout.contains("won't boot"),
        "status flags the unservable kind and names the fix: {stdout}"
    );
}

/// The packaged binary's first-contact contract: `--help` and
/// `--version` answer from a bare environment — no trust anchors, no
/// identity minted, nothing booted — and an unknown subcommand refuses
/// loudly instead of silently starting to serve.
#[tokio::test]
async fn help_version_and_unknown_arguments_answer_without_booting() {
    let home = tempfile::tempdir().unwrap();
    let bare = |arg: &str| {
        Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"))
            .env_clear()
            .env("COVENANT_COMPUTE_NODE_HOME", home.path())
            .arg(arg)
            .output()
    };

    let help = bare("--help").await.unwrap();
    assert!(help.status.success(), "--help exits 0");
    let text = String::from_utf8_lossy(&help.stdout);
    for token in [
        "setup",
        "earnings verify",
        "bond unbond",
        "COVENANT_COMPUTE_COORDINATOR_PUBKEY",
    ] {
        assert!(text.contains(token), "usage names {token}: {text}");
    }
    assert!(
        !home.path().join("identity.json").exists(),
        "asking for help must not mint an operator identity"
    );

    let version = bare("--version").await.unwrap();
    assert!(version.status.success(), "--version exits 0");
    assert_eq!(
        String::from_utf8_lossy(&version.stdout),
        format!("covenant-compute-node {}\n", env!("CARGO_PKG_VERSION")),
    );

    let unknown = bare("frobnicate").await.unwrap();
    assert!(!unknown.status.success(), "an unknown subcommand refuses");
    let err = String::from_utf8_lossy(&unknown.stderr);
    assert!(
        err.contains("unknown subcommand") && err.contains("--help"),
        "{err}"
    );
}

/// The subcommand-level `--help`/`-h` a first-time operator reaches for
/// after seeing `setup [flags]` in the top-level usage: every subcommand
/// answers with usage on stdout and exits 0 — never rejecting the flag as
/// unknown, and never (as `status` once did) silently running the command
/// in place of the help it was asked for. Answered from the same bare,
/// identity-free environment as the top-level help.
#[tokio::test]
async fn subcommand_help_answers_on_stdout_without_booting() {
    let home = tempfile::tempdir().unwrap();
    let help = |args: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"));
        cmd.env_clear()
            .env("COVENANT_COMPUTE_NODE_HOME", home.path())
            // Unroutable on purpose: a subcommand that booted instead of
            // answering would try to reach it rather than exit clean.
            .env("COVENANT_COMPUTE_COORDINATOR_URL", "http://127.0.0.1:1")
            .args(args);
        cmd.output()
    };

    // setup answers with its own flag-level usage, not the top-level table.
    let setup = help(&["setup", "--help"]).await.unwrap();
    assert!(setup.status.success(), "setup --help exits 0");
    let text = String::from_utf8_lossy(&setup.stdout);
    assert!(
        text.contains("--coordinator-url") && text.contains("--executor"),
        "setup --help prints the setup flags: {text}"
    );

    // status would otherwise read the coordinator; -h must answer with
    // usage instead of running the command.
    let status = help(&["status", "-h"]).await.unwrap();
    assert!(status.status.success(), "status -h exits 0");
    let text = String::from_utf8_lossy(&status.stdout);
    assert!(
        text.contains("Usage:") && text.contains("serve jobs"),
        "status -h answers with usage, not a status read: {text}"
    );

    // bond, the other subcommand that used to reject the flag outright.
    let bond = help(&["bond", "--help"]).await.unwrap();
    assert!(bond.status.success(), "bond --help exits 0");
    assert!(
        String::from_utf8_lossy(&bond.stdout).contains("bond unbond"),
        "bond --help answers with usage"
    );

    assert!(
        !home.path().join("identity.json").exists(),
        "asking a subcommand for help must not mint an operator identity"
    );
}

/// A serve boot with no configuration — no node.env, no trust anchors in
/// the environment — is a new operator's most likely first mistake:
/// running the node before `setup`. It must fail fast and name setup, not
/// leave them decoding a bare "variable unset".
#[tokio::test]
async fn serving_without_configuration_points_at_setup() {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_covenant-compute-node"))
        .env_clear()
        .env("COVENANT_COMPUTE_NODE_HOME", home.path())
        .output()
        .await
        .unwrap();
    assert!(
        !out.status.success(),
        "an unconfigured serve boot must refuse"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("setup"),
        "the refusal must point a new operator at setup: {err}"
    );
}

/// `status` names why unpaid rows concluded — the faults count gets
/// its reasons, split into what faults this node's standing and what
/// was the buyer walking away — and names WHICH job a disputed count
/// is about, complaint included. Real wire end to end: an assigned
/// offer this identity never polls is cancelled by its buyer (no
/// fault), a second is rejected through the operator session (a
/// fault), a third completes and is disputed, and the spawned
/// binary's own signed books read renders all three. A home with no
/// history prints none of these lines.
#[tokio::test]
async fn status_names_the_refund_reasons_behind_the_faults_count() {
    use covenant_compute_protocol::{
        CancelRequest, CapabilityProfile, DisputeRequest, HardwareClass, JobMeter,
        JobResultMessage, PriceAsk, PriceUnit, RegisterRequest, WorkReceiptPayload,
    };

    let (url, _coordinator_pubkey, rail, _coordinator_home) = coordinator().await;
    let http = reqwest::Client::new();

    // The operator identity the spawned `status` will load — minted
    // here so the jobs can conclude before the binary ever runs.
    let node_home = tempfile::tempdir().unwrap();
    let identity =
        LocalIdentity::load_or_create(&node_home.path().join("identity.json"), "operator@compute")
            .unwrap();
    let profile = CapabilityProfile {
        operator: identity.agent_id(),
        hardware: HardwareClass::CpuOnly,
        vram_gb: 0,
        models_served: vec!["any".into()],
        job_kinds: vec![JobKind::InferenceCall],
        price: PriceAsk {
            unit: PriceUnit::PerJob,
            micro_usdc: 1_000,
        },
        tee_capable: false,
        kind_prices: Vec::new(),
        kind_models: Vec::new(),
    };
    let session = http
        .post(format!("{url}/federation/operators/register"))
        .json(
            &RegisterRequest::sign(profile, bs58::encode([9u8; 32]).into_string(), &identity)
                .unwrap(),
        )
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()["operator_session"]
        .as_str()
        .unwrap()
        .to_string();

    let buyer = LocalIdentity::generate("buyer@test");
    let config = BuyerConfig {
        coordinator_url: url.clone(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    rail.preload(VerifiedDeposit {
        deposit_id: "reasons-deposit".into(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc: 100_000,
    });
    claim_deposit(&http, &config, &buyer, "reasons-deposit")
        .await
        .unwrap();
    let submit = |input: &str| {
        covenant_compute_buyer::sign_envelope(
            &config,
            &buyer,
            JobRequest {
                kind: JobKind::InferenceCall,
                input: vec![Content::text(input)],
                model: None,
                gpu_class: None,
                min_vram_gb: None,
                min_reputation_bps: None,
                price_micro_usdc: 1_000,
                deadline_ms: 60_000,
            },
        )
        .unwrap()
    };

    // Fate one: assigned to this identity (which never polls), then
    // withdrawn by the buyer — refunded, nobody's fault.
    let cancelled = submit("cancelled work");
    let cancelled_id = cancelled.payload.job_id;
    let resp = http
        .post(format!("{url}/federation/jobs"))
        .json(&cancelled)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let cancel = CancelRequest::sign(
        buyer.agent_id(),
        cancelled_id,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
        &buyer,
    )
    .unwrap();
    let resp = http
        .post(format!("{url}/federation/jobs/{cancelled_id}/cancel"))
        .json(&cancel)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // Fate two: rejected through the operator's own session — the
    // fault its standing carries.
    let rejected = submit("rejected work");
    let rejected_id = rejected.payload.job_id;
    let resp = http
        .post(format!("{url}/federation/jobs"))
        .json(&rejected)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let resp = http
        .post(format!("{url}/federation/jobs/{rejected_id}/accept"))
        .bearer_auth(&session)
        .json(&serde_json::json!({
            "decision": "reject",
            "job_id": rejected_id,
            "reason": "at capacity",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // Fate three: served and paid, then disputed by its buyer. The
    // standing's disputed count only becomes actionable when `status`
    // names the job and shows the complaint.
    let disputed = submit("disputed work");
    let disputed_id = disputed.payload.job_id;
    let resp = http
        .post(format!("{url}/federation/jobs"))
        .json(&disputed)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let output = vec![Content::text("served output")];
    let receipt = SignedWorkReceipt::sign(
        WorkReceiptPayload {
            job_id: disputed_id,
            operator: identity.agent_id(),
            job_hash_hex: "aa".repeat(32),
            result_hash_hex: covenant_compute_protocol::output_hash_hex(&output),
            meter: JobMeter {
                wall_ms: 5,
                tokens_in: None,
                tokens_out: None,
                gpu_seconds: None,
                finish_reason: None,
            },
            price_micro_usdc: 1_000,
            status: covenant_a2a::A2ATaskStatus::Ok,
            executed_at_ms: now_ms,
            node_audit_root_hex: "cc".repeat(32),
        },
        &identity,
    )
    .unwrap();
    let resp = http
        .post(format!("{url}/federation/jobs/{disputed_id}/result"))
        .json(&JobResultMessage { receipt, output })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let complaint = DisputeRequest::sign(
        buyer.agent_id(),
        disputed_id,
        "the output was stale".into(),
        now_ms,
        &buyer,
    )
    .unwrap();
    let resp = http
        .post(format!("{url}/federation/jobs/{disputed_id}/dispute"))
        .json(&complaint)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // A last job sits live at Offered — in-flight work is not an
    // unpaid conclusion and must stay out of the summary.
    let live = submit("live work");
    let resp = http
        .post(format!("{url}/federation/jobs"))
        .json(&live)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);

    let status = cli(node_home.path(), &url, &["status"]).await;
    assert!(
        status.contains(
            "unpaid rows:   buyer_cancelled 1 (no fault — the buyer walked away), \
             operator_rejected 1 (faults your standing)"
        ),
        "{status}"
    );
    assert!(
        !status.contains("offered"),
        "a live offer is not an unpaid conclusion: {status}"
    );
    assert!(
        status.contains(&format!(
            "disputed:      {disputed_id} — \"the output was stale\""
        )),
        "the disputed job is named with its complaint: {status}"
    );

    // A home with no unpaid conclusions keeps both lines silent.
    let clean_home = tempfile::tempdir().unwrap();
    let clean = cli(clean_home.path(), &url, &["status"]).await;
    assert!(!clean.contains("unpaid rows:"), "{clean}");
    assert!(!clean.contains("disputed:      "), "{clean}");
}
