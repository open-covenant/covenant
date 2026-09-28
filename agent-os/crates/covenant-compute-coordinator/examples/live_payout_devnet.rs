//! Live devnet proof: the coordinator's real release -> `SidecarPayout`
//! path moves a real, confirmed SPL transfer into an operator's ATA —
//! no mocks, no facilitator, devnet only.
//!
//! Opt-in only: examples are never run by `cargo test`/`cargo build
//! --workspace`, only by an explicit `cargo run --example`. Needs:
//!   - the `solana` / `solana-keygen` / `spl-token` CLI on PATH
//!   - network egress to a devnet RPC (defaults to
//!     <https://api.devnet.solana.com>; override with
//!     COVENANT_COMPUTE_DEVNET_RPC_URL if that faucet/RPC is rate-limited)
//!   - the `covenant-x402-signer` sidecar already built in its OWN
//!     standalone workspace (it deliberately is not a member of this
//!     workspace — see `covenant-x402-signer/Cargo.toml`):
//!     `(cd ../covenant-x402-signer && cargo build)`
//!
//! Run from `agent-os/`:
//!   cargo run -p covenant-compute-coordinator --example live_payout_devnet
//!
//! Every keypair here is freshly generated into a throwaway temp dir.
//! Every `solana`/`spl-token` subprocess call below also runs with
//! `HOME` redirected into that same temp dir, so nothing in this file
//! can read or write `~/.config/solana` even if a flag is missed —
//! belt-and-suspenders on top of the explicit `--fee-payer`/`--owner`
//! flags each call already passes. All funds are devnet play-money: a
//! free airdrop, and a throwaway SPL mint standing in for USDC.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
use covenant_audit::{AuditLog, InMemoryAuditLog};
use covenant_compute_coordinator::{
    router, AuditReputationSource, CoordinatorConfig, CoordinatorState, SidecarPayout,
    SidecarPayoutConfig,
};
use covenant_compute_node::{
    Coordinator, EchoExecutor, HttpCoordinatorClient, InMemoryEarningsLedger, Node, NodeConfig,
};
use covenant_compute_protocol::{
    CapabilityProfile, CapabilityRequirement, EscrowStatus, FederationEscrow, FundingSource,
    HardwareClass, JobEnvelopePayload, JobKind, PriceAsk, PriceUnit, RegisterRequest,
    SignedJobEnvelope,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use uuid::Uuid;

/// 0.25 "USDC" at 6 decimals — the throwaway mint below is created
/// with 6 decimals so this maps 1:1 onto SPL base units, same as real
/// USDC.
const JOB_PRICE_MICRO_USDC: u64 = 250_000;

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A CLI invocation with `HOME` redirected into the isolated scratch
/// dir — see the module docs for why this matters.
fn isolated(home: &Path, program: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.env("HOME", home);
    cmd
}

fn run(cmd: &mut Command, what: &str) -> String {
    let output = cmd.output().unwrap_or_else(|e| panic!("spawn {what}: {e}"));
    if !output.status.success() {
        panic!(
            "{what} failed ({}):\nstdout: {}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn keygen(home: &Path, path: &Path) -> String {
    run(
        isolated(home, "solana-keygen")
            .args([
                "new",
                "--no-bip39-passphrase",
                "--force",
                "--silent",
                "--outfile",
            ])
            .arg(path),
        "solana-keygen new",
    );
    run(
        isolated(home, "solana-keygen").arg("pubkey").arg(path),
        "solana-keygen pubkey",
    )
}

fn airdrop(home: &Path, rpc_url: &str, pubkey: &str) {
    for attempt in 1..=8 {
        let output = isolated(home, "solana")
            .args(["airdrop", "1", pubkey, "--url", rpc_url])
            .output()
            .expect("spawn solana airdrop");
        let stdout = String::from_utf8_lossy(&output.stdout);
        if output.status.success() && stdout.contains("Signature:") {
            println!("airdropped 1 SOL to {pubkey} ({})", stdout.trim());
            return;
        }
        eprintln!(
            "airdrop attempt {attempt}/8: {}{}",
            stdout.trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        std::thread::sleep(Duration::from_secs(if attempt <= 3 { 5 } else { 20 }));
    }
    panic!(
        "devnet airdrop to {pubkey} did not succeed after 8 attempts — the faucet is likely \
         rate-limited for this egress IP right now; retry later or from a different network. \
         This is an environmental limit, not a code issue: re-run this example once the \
         faucet cools (`cargo run -p covenant-compute-coordinator --example \
         live_payout_devnet`)."
    );
}

/// Parses `spl-token balance --output json`'s `uiAmountString` (falls
/// back to treating the raw stdout as a bare float if the shape ever
/// changes — this call is diagnostic, not the guardrail).
fn parse_ui_amount(json_or_plain: &str) -> f64 {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_or_plain) {
        for pointer in ["/uiAmountString", "/value/uiAmountString"] {
            if let Some(s) = v.pointer(pointer).and_then(|x| x.as_str()) {
                return s.parse().expect("uiAmountString parses as f64");
            }
        }
    }
    json_or_plain
        .parse()
        .unwrap_or_else(|_| panic!("could not parse balance output: {json_or_plain:?}"))
}

async fn spawn_coordinator(state: CoordinatorState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router(state)).await.unwrap();
    });
    format!("http://{addr}")
}

#[tokio::main]
async fn main() {
    let rpc_url = std::env::var("COVENANT_COMPUTE_DEVNET_RPC_URL")
        .unwrap_or_else(|_| "https://api.devnet.solana.com".to_string());
    assert!(
        !rpc_url.contains("mainnet"),
        "refusing to run against anything that looks like mainnet: {rpc_url}"
    );

    let home =
        std::env::temp_dir().join(format!("covenant-compute-payout-devnet-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&home).expect("create isolated scratch home");
    println!(
        "isolated scratch home (never touches ~/.config/solana): {}",
        home.display()
    );

    let signer_binary = std::env::var("COVENANT_X402_SIGNER_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../covenant-x402-signer/target/debug/covenant-x402-signer")
        });
    assert!(
        signer_binary.exists(),
        "sidecar binary not found at {} — build it first: (cd ../covenant-x402-signer && cargo \
         build), or point COVENANT_X402_SIGNER_BINARY at an already-built one",
        signer_binary.display()
    );

    // --- devnet setup: two fresh throwaway wallets, an airdrop, and a
    // throwaway SPL mint standing in for USDC ---
    let funder_path = home.join("coordinator-funder.json");
    let funder_pubkey = keygen(&home, &funder_path);
    println!("coordinator funder wallet (devnet, throwaway): {funder_pubkey}");

    let operator_payout_path = home.join("operator-payout.json");
    let operator_payout_pubkey = keygen(&home, &operator_payout_path);
    println!("operator payout wallet (devnet, throwaway): {operator_payout_pubkey}");

    airdrop(&home, &rpc_url, &funder_pubkey);

    let mint_path = home.join("mint.json");
    let mint_pubkey = keygen(&home, &mint_path);
    run(
        isolated(&home, "spl-token")
            .args(["--url", &rpc_url, "--fee-payer"])
            .arg(&funder_path)
            .arg("create-token")
            .args(["--decimals", "6", "--mint-authority", &funder_pubkey])
            .arg(&mint_path),
        "spl-token create-token",
    );
    println!("created throwaway devnet SPL mint (stand-in for USDC): {mint_pubkey}");

    run(
        isolated(&home, "spl-token")
            .args(["--url", &rpc_url, "--fee-payer"])
            .arg(&funder_path)
            .args(["create-account", &mint_pubkey, "--owner", &funder_pubkey]),
        "spl-token create-account (funder)",
    );

    // Fund the coordinator's own payout wallet with enough throwaway
    // token to cover the job below (mint authority = the funder
    // keypair itself, set at create-token time).
    let mint_amount_tokens = (JOB_PRICE_MICRO_USDC * 10) as f64 / 1_000_000.0;
    run(
        isolated(&home, "spl-token")
            .args(["--url", &rpc_url, "--fee-payer"])
            .arg(&funder_path)
            .arg("mint")
            .arg(&mint_pubkey)
            .arg(mint_amount_tokens.to_string())
            .arg("--mint-authority")
            .arg(&funder_path)
            .args(["--recipient-owner", &funder_pubkey]),
        "spl-token mint (fund the coordinator's payout wallet)",
    );

    // --- the coordinator, wired with the REAL SidecarPayout ---
    let identity = LocalIdentity::generate("coordinator@live-payout-devnet");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(SidecarPayout::new(SidecarPayoutConfig {
        signer_binary: signer_binary.clone(),
        rpc_url: rpc_url.clone(),
        funding_keypair_path: funder_path.display().to_string(),
        mint: mint_pubkey.clone(),
        cap_micro_usdc: JOB_PRICE_MICRO_USDC * 100,
        obligation_cap_micro_usdc: 0,
    }));
    let config = CoordinatorConfig {
        long_poll_timeout: Duration::from_secs(5),
        default_funding_source: FundingSource::Organic,
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::new(identity, config, reputation, payout.clone(), audit);
    let state_handle = state.clone();
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state).await;

    // --- register the operator with its REAL devnet payout wallet ---
    let operator_identity = LocalIdentity::generate("operator@live-payout-devnet");
    let profile = CapabilityProfile {
        operator: operator_identity.agent_id(),
        hardware: HardwareClass::CpuOnly,
        vram_gb: 0,
        models_served: vec!["any".into()],
        job_kinds: vec![JobKind::BatchJob],
        price: PriceAsk {
            unit: PriceUnit::PerJob,
            micro_usdc: JOB_PRICE_MICRO_USDC,
        },
        tee_capable: false,
    };
    // A generous single-call timeout: the node's HTTP client caps every
    // call (including /result) at a fixed 10s regardless of this
    // argument (covenant-compute-node::http_client::DEFAULT_CALL_TIMEOUT)
    // — see the note below where run_once()'s result is handled.
    let coordinator_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        1,
    ));
    let register_req = RegisterRequest::sign(
        profile.clone(),
        operator_payout_pubkey.clone(),
        &operator_identity,
    )
    .expect("sign register request");
    let register_resp = coordinator_client
        .register(register_req)
        .await
        .expect("register with the coordinator");
    assert!(
        register_resp.accepted,
        "operator registration must be accepted"
    );

    let node_audit = Arc::new(InMemoryAuditLog::new());
    let earnings = Arc::new(InMemoryEarningsLedger::new());
    let executor = Arc::new(EchoExecutor);
    let node = Node::new(
        operator_identity,
        profile,
        coordinator_client,
        executor,
        earnings,
        node_audit,
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    );

    // --- a real signed buyer job envelope, submitted over HTTP ---
    let buyer_identity = LocalIdentity::generate("buyer@live-payout-devnet");
    let job_id = Uuid::new_v4();
    let now_ms = epoch_ms();
    let payload = JobEnvelopePayload {
        job_id,
        buyer: buyer_identity.agent_id(),
        kind: JobKind::BatchJob,
        capability_requirement: CapabilityRequirement {
            gpu_class: None,
            min_vram_gb: None,
            model_id: None,
            kind: JobKind::BatchJob,
            max_duration_secs: 30,
            min_reputation_bps: None,
        },
        input: vec![Content::text("live devnet payout proof")],
        price_micro_usdc: JOB_PRICE_MICRO_USDC,
        deadline_ms: 120_000,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "live-payout-devnet"),
        issued_at_ms: now_ms,
        referral_code: None,
        stream: false,
    };
    let envelope = SignedJobEnvelope::sign(payload, &buyer_identity).expect("sign envelope");

    let http = reqwest::Client::new();
    let submit_resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .expect("submit job");
    assert_eq!(
        submit_resp.status(),
        reqwest::StatusCode::ACCEPTED,
        "job submission must be accepted"
    );
    println!("job {job_id} submitted (price {JOB_PRICE_MICRO_USDC} micro-usdc); running the node loop...");

    // node.run_once() drives admit -> execute -> sign receipt ->
    // POST /result, and submit_result awaits SidecarPayout::pay(...)
    // synchronously before answering. A real devnet confirm can take
    // longer than the node HTTP client's fixed 10s call timeout, so a
    // client-side timeout/retry here does NOT mean the payout failed
    // server-side — it means exactly the seam this slice flagged
    // rather than decided (sync-in-handler vs. queued). Don't trust
    // run_once()'s Ok/Err for the proof; poll the payout record below.
    match node.run_once().await {
        Ok(Some(outcome)) => {
            println!("node run_once completed for job {}", outcome.job_id);
            outcome.receipt.verify().expect("receipt must verify");
        }
        Ok(None) => {
            println!("WARNING: run_once saw no offer (unexpected — matching may have failed)")
        }
        Err(e) => println!(
            "run_once returned {e} — likely just the node HTTP client's 10s call timeout \
             while the coordinator's payout was still confirming server-side; polling the \
             payout record directly instead of trusting this."
        ),
    }

    let mut record = None;
    for _ in 0..60 {
        if let Some(r) = payout.record_for(job_id) {
            record = Some(r);
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let record = record.expect("payout must complete within 30s of the job landing");
    let signature = record
        .tx_signature
        .clone()
        .expect("a SidecarPayout record always carries a tx signature on success");

    assert_eq!(record.amount_micro_usdc, JOB_PRICE_MICRO_USDC);
    assert_eq!(record.payout_address, operator_payout_pubkey);
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released,
        "escrow must be released before a payout is ever pushed"
    );

    let balance_json = run(
        isolated(&home, "spl-token")
            .args([
                "--url",
                &rpc_url,
                "--output",
                "json",
                "balance",
                &mint_pubkey,
                "--owner",
            ])
            .arg(&operator_payout_pubkey),
        "spl-token balance (operator)",
    );
    let balance_units = parse_ui_amount(&balance_json);
    let expected_units = JOB_PRICE_MICRO_USDC as f64 / 1_000_000.0;
    assert!(
        (balance_units - expected_units).abs() < 1e-9,
        "operator devnet balance {balance_units} does not match the paid amount {expected_units}"
    );

    // The trustless half: a third party holding only the signed
    // receipt and the tx signature re-derives the memo, fetches the
    // transaction, and checks the chain paid for exactly this work —
    // no coordinator, no registry, no spl-token CLI.
    let receipt = state_handle
        .jobs()
        .get(job_id)
        .expect("job record")
        .receipt
        .clone()
        .expect("a paid job carries its receipt");
    let http = reqwest::Client::new();
    let mut tx = serde_json::Value::Null;
    for _ in 0..20 {
        tx = covenant_compute_buyer::fetch_payout_transaction(&http, &rpc_url, &signature)
            .await
            .expect("getTransaction");
        if !tx.is_null() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let proof = covenant_compute_buyer::verify_payout_onchain(&receipt, &tx)
        .expect("the on-chain transfer must verify against the receipt");
    assert_eq!(proof.amount_micro_usdc, JOB_PRICE_MICRO_USDC);
    assert_eq!(proof.mint_b58, mint_pubkey);
    assert_eq!(proof.recipient_owner_b58, operator_payout_pubkey);

    println!("\n=== LIVE DEVNET PAYOUT PROOF ===");
    println!("job_id: {job_id}");
    println!("mint (throwaway devnet token standing in for USDC): {mint_pubkey}");
    println!("operator payout wallet: {operator_payout_pubkey}");
    println!("amount: {JOB_PRICE_MICRO_USDC} micro-usdc ({expected_units} tokens)");
    println!("tx signature: {signature}");
    println!("memo on-chain: {}", receipt.payout_memo());
    println!("explorer: https://explorer.solana.com/tx/{signature}?cluster=devnet");
    println!("operator balance confirmed on-chain: {balance_units} tokens");
    println!(
        "verify_payout_onchain: chain shows {} micro-usdc of {} to {} for this receipt's memo",
        proof.amount_micro_usdc, proof.mint_b58, proof.recipient_owner_b58
    );
}
