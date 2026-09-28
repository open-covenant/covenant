//! Live devnet proof: a buyer funds its pre-paid balance through the
//! real `SolanaRpcRail` — an actual SPL transfer with a
//! `compute-buyer:<pubkey>` memo, verified off devnet by signature,
//! credited once, and then spent on a real job — no mocks, devnet only.
//!
//! This is the inbound mirror of `live_payout_devnet`: that example
//! proves money leaves the coordinator correctly, this one proves money
//! arrives. The 402 half is the point — prefunding is ENFORCED here, so
//! the job is refused until the deposit lands, and admitted right after.
//!
//! Opt-in only: examples are never run by `cargo test`/`cargo build
//! --workspace`, only by an explicit `cargo run --example`. Needs:
//!   - the `solana` / `solana-keygen` / `spl-token` CLI on PATH
//!   - network egress to a devnet RPC (defaults to
//!     <https://api.devnet.solana.com>; override with
//!     COVENANT_COMPUTE_DEVNET_RPC_URL if that faucet/RPC is rate-limited)
//!
//! Run from `agent-os/`:
//!   cargo run -p covenant-compute-coordinator --example live_deposit_devnet
//!
//! Every keypair here is freshly generated into a throwaway temp dir,
//! and every `solana`/`spl-token` subprocess runs with `HOME` redirected
//! into that same dir, so nothing here can read or write
//! `~/.config/solana`. All funds are devnet play-money: a free airdrop,
//! and a throwaway SPL mint standing in for USDC.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
use covenant_audit::{AuditLog, InMemoryAuditLog};
use covenant_compute_coordinator::{
    router, AuditReputationSource, CoordinatorConfig, CoordinatorState, InboundRail, MockPayout,
    SolanaRpcRail,
};
use covenant_compute_node::{
    Coordinator, EchoExecutor, HttpCoordinatorClient, InMemoryEarningsLedger, Node, NodeConfig,
};
use covenant_compute_protocol::{
    CapabilityProfile, CapabilityRequirement, HardwareClass, JobEnvelopePayload, JobKind, PriceAsk,
    PriceUnit, RegisterRequest, SignedJobEnvelope, DEPOSIT_MEMO_PREFIX,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use uuid::Uuid;

/// 0.5 "USDC" deposited, 0.1 spent — both at 6 decimals so micro-USDC
/// maps 1:1 onto base units of the throwaway mint, same as real USDC.
const DEPOSIT_MICRO_USDC: u64 = 500_000;
const JOB_PRICE_MICRO_USDC: u64 = 100_000;

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
         rate-limited for this egress IP right now. This is an environmental limit, not a code \
         issue: re-run this example once the faucet cools (`cargo run -p \
         covenant-compute-coordinator --example live_deposit_devnet`)."
    );
}

async fn spawn_coordinator(state: CoordinatorState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router(state)).await.unwrap();
    });
    format!("http://{addr}")
}

fn signed_job(buyer: &LocalIdentity, idem_key: &str) -> SignedJobEnvelope {
    let job_id = Uuid::new_v4();
    let payload = JobEnvelopePayload {
        job_id,
        buyer: buyer.agent_id(),
        kind: JobKind::BatchJob,
        capability_requirement: CapabilityRequirement {
            gpu_class: None,
            min_vram_gb: None,
            model_id: None,
            kind: JobKind::BatchJob,
            max_duration_secs: 30,
            min_reputation_bps: None,
        },
        input: vec![Content::text("live devnet deposit proof")],
        price_micro_usdc: JOB_PRICE_MICRO_USDC,
        deadline_ms: 120_000,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, idem_key),
        issued_at_ms: epoch_ms(),
        referral_code: None,
        stream: false,
    };
    SignedJobEnvelope::sign(payload, buyer).expect("sign envelope")
}

#[tokio::main]
async fn main() {
    let rpc_url = std::env::var("COVENANT_COMPUTE_DEVNET_RPC_URL")
        .unwrap_or_else(|_| "https://api.devnet.solana.com".to_string());
    assert!(
        !rpc_url.contains("mainnet"),
        "refusing to run against anything that looks like mainnet: {rpc_url}"
    );

    let home = std::env::temp_dir().join(format!(
        "covenant-compute-deposit-devnet-{}",
        Uuid::new_v4()
    ));
    std::fs::create_dir_all(&home).expect("create isolated scratch home");
    println!(
        "isolated scratch home (never touches ~/.config/solana): {}",
        home.display()
    );

    // --- devnet setup: a payer wallet, the coordinator's deposit
    // owner, a throwaway mint, and tokens for the payer to deposit ---
    let payer_path = home.join("buyer-payer.json");
    let payer_pubkey = keygen(&home, &payer_path);
    println!("buyer's paying wallet (devnet, throwaway): {payer_pubkey}");

    let deposit_owner_path = home.join("deposit-owner.json");
    let deposit_owner_pubkey = keygen(&home, &deposit_owner_path);
    println!("coordinator deposit owner (devnet, throwaway): {deposit_owner_pubkey}");

    airdrop(&home, &rpc_url, &payer_pubkey);

    let mint_path = home.join("mint.json");
    let mint_pubkey = keygen(&home, &mint_path);
    run(
        isolated(&home, "spl-token")
            .args(["--url", &rpc_url, "--fee-payer"])
            .arg(&payer_path)
            .arg("create-token")
            .args(["--decimals", "6", "--mint-authority", &payer_pubkey])
            .arg(&mint_path),
        "spl-token create-token",
    );
    println!("created throwaway devnet SPL mint (stand-in for USDC): {mint_pubkey}");
    run(
        isolated(&home, "spl-token")
            .args(["--url", &rpc_url, "--fee-payer"])
            .arg(&payer_path)
            .args(["create-account", &mint_pubkey, "--owner", &payer_pubkey]),
        "spl-token create-account (payer)",
    );
    let mint_amount_tokens = (DEPOSIT_MICRO_USDC * 2) as f64 / 1_000_000.0;
    run(
        isolated(&home, "spl-token")
            .args(["--url", &rpc_url, "--fee-payer"])
            .arg(&payer_path)
            .arg("mint")
            .arg(&mint_pubkey)
            .arg(mint_amount_tokens.to_string())
            .arg("--mint-authority")
            .arg(&payer_path)
            .args(["--recipient-owner", &payer_pubkey]),
        "spl-token mint (fund the payer)",
    );

    // --- the coordinator: REAL SolanaRpcRail, prefunding ENFORCED ---
    let identity = LocalIdentity::generate("coordinator@live-deposit-devnet");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let rail: Arc<dyn InboundRail> = Arc::new(SolanaRpcRail::new(
        rpc_url.clone(),
        deposit_owner_pubkey.clone(),
        mint_pubkey.clone(),
    ));
    println!("inbound rail: {}", rail.describe());
    let config = CoordinatorConfig {
        long_poll_timeout: Duration::from_secs(5),
        require_prefunded_buyers: true,
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::with_journal(
        identity,
        config,
        reputation,
        Arc::new(MockPayout::new()),
        audit,
        &home.join("journal.jsonl"),
        Some(rail),
    )
    .await
    .expect("build coordinator state");
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state).await;

    // The buyer whose balance the deposit funds — its pubkey rides the
    // transfer memo, nothing else ties the payment to it.
    let buyer_identity = LocalIdentity::generate("buyer@live-deposit-devnet");
    let buyer_pubkey = buyer_identity.agent_id().pubkey_base58();
    let http = reqwest::Client::new();

    // --- the 402 half: with prefunding enforced and no deposit, the
    // job is refused before any operator is consulted ---
    let refused = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&signed_job(&buyer_identity, "live-deposit-devnet-before"))
        .send()
        .await
        .expect("submit pre-deposit job");
    assert_eq!(
        refused.status(),
        reqwest::StatusCode::PAYMENT_REQUIRED,
        "an unfunded buyer must be refused with 402"
    );
    println!("pre-deposit job refused with 402, as enforced prefunding demands");

    // --- the real deposit: an SPL transfer to the deposit owner's ATA
    // carrying the memo that names the buyer it funds ---
    let deposit_tokens = DEPOSIT_MICRO_USDC as f64 / 1_000_000.0;
    let memo = format!("{DEPOSIT_MEMO_PREFIX}{buyer_pubkey}");
    let transfer_out = run(
        isolated(&home, "spl-token")
            .args(["--url", &rpc_url, "--fee-payer"])
            .arg(&payer_path)
            .arg("transfer")
            .arg(&mint_pubkey)
            .arg(deposit_tokens.to_string())
            .arg(&deposit_owner_pubkey)
            .args(["--owner"])
            .arg(&payer_path)
            .args(["--fund-recipient", "--allow-unfunded-recipient"])
            .args(["--with-memo", &memo]),
        "spl-token transfer (the deposit)",
    );
    let deposit_signature = transfer_out
        .lines()
        .find_map(|l| l.trim().strip_prefix("Signature: "))
        .unwrap_or_else(|| panic!("no signature in spl-token transfer output:\n{transfer_out}"))
        .to_string();
    println!("deposit transfer sent: {deposit_signature} (memo: {memo})");

    // --- claim it. The rail reads finality off the chain, so poll
    // through the 404s until the transaction finalizes ---
    let claim = serde_json::json!({
        "buyer_pubkey_b58": buyer_pubkey,
        "deposit_id": deposit_signature,
    });
    let mut credited: Option<serde_json::Value> = None;
    for attempt in 1..=45 {
        let resp = http
            .post(format!("{base_url}/federation/buyers/deposit"))
            .json(&claim)
            .send()
            .await
            .expect("claim deposit");
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            // Not finalized yet.
            if attempt % 5 == 0 {
                println!("waiting for finality ({attempt}/45)...");
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        assert!(
            resp.status().is_success(),
            "deposit claim failed: {} {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        );
        credited = Some(resp.json().await.expect("decode deposit view"));
        break;
    }
    let credited = credited.expect("the deposit must finalize and credit within ~90s");
    assert_eq!(credited["credited"], true, "first claim credits");
    assert_eq!(credited["amount_micro_usdc"], DEPOSIT_MICRO_USDC);
    println!("deposit credited: {credited}");

    // An honest retry acknowledges without double-crediting.
    let reclaim: serde_json::Value = http
        .post(format!("{base_url}/federation/buyers/deposit"))
        .json(&claim)
        .send()
        .await
        .expect("re-claim deposit")
        .json()
        .await
        .expect("decode re-claim");
    assert_eq!(reclaim["credited"], false, "re-claim never double-credits");

    // --- the balance is real: register a node, resubmit, get served ---
    let operator_identity = LocalIdentity::generate("operator@live-deposit-devnet");
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
    let coordinator_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        1,
    ));
    // Payout-to-self: this example exercises the deposit path only, so
    // the payout address just has to be payable in shape — registration
    // refuses anything that isn't a 32-byte key.
    let register_req = RegisterRequest::sign(
        profile.clone(),
        operator_identity.agent_id().pubkey_base58(),
        &operator_identity,
    )
    .expect("sign register request");
    coordinator_client
        .register(register_req)
        .await
        .expect("register operator");
    let node = Node::new(
        operator_identity,
        profile,
        coordinator_client,
        Arc::new(EchoExecutor),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    );

    let funded_job = signed_job(&buyer_identity, "live-deposit-devnet-after");
    let job_id = funded_job.payload.job_id;
    let accepted = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&funded_job)
        .send()
        .await
        .expect("submit funded job");
    assert_eq!(
        accepted.status(),
        reqwest::StatusCode::ACCEPTED,
        "the deposited balance must clear the same 402 gate"
    );
    let outcome = node
        .run_once()
        .await
        .expect("node serves the funded job")
        .expect("an offer was pending");
    assert_eq!(outcome.job_id, job_id);
    outcome.receipt.verify().expect("receipt verifies");

    // --- the signed balance read shows deposit minus spend ---
    let balance_path = format!("/federation/buyers/{buyer_pubkey}/balance");
    let signed_at_ms = epoch_ms();
    let signature =
        covenant_compute_protocol::sign_read(&buyer_identity, &balance_path, signed_at_ms)
            .expect("sign balance read");
    let funds: serde_json::Value = http
        .get(format!("{base_url}{balance_path}"))
        .header(
            covenant_compute_protocol::READ_SIGNED_AT_HEADER,
            signed_at_ms.to_string(),
        )
        .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, signature)
        .send()
        .await
        .expect("balance read")
        .json()
        .await
        .expect("decode balance");
    assert_eq!(funds["deposited_micro_usdc"], DEPOSIT_MICRO_USDC);
    assert_eq!(funds["charged_micro_usdc"], JOB_PRICE_MICRO_USDC);
    assert_eq!(
        funds["available_micro_usdc"],
        DEPOSIT_MICRO_USDC - JOB_PRICE_MICRO_USDC
    );

    // --- A3's money-out: the remainder walks back out. The books
    // arbitrate the withdrawal against the SAME devnet-verified
    // deposit the jobs spend from — overdraw refused, the remainder
    // debited exactly once, nothing trapped. The transfer itself rides
    // the payout backend (mock here; the real-transfer half is
    // live_payout_devnet's sidecar proof), memo-tagged so the eventual
    // on-chain transaction names this withdrawal.
    let remainder = DEPOSIT_MICRO_USDC - JOB_PRICE_MICRO_USDC;
    let overdraw = covenant_compute_buyer::withdraw(
        &http,
        &covenant_compute_buyer::BuyerConfig {
            coordinator_url: base_url.clone(),
            poll_interval: Duration::from_millis(200),
            referral_code: None,
            rpc_url: None,
        },
        &buyer_identity,
        Uuid::new_v4(),
        remainder + 1,
        &payer_pubkey,
    )
    .await
    .expect_err("withdrawing more than the remainder must be refused");
    assert!(
        overdraw.to_string().contains("402"),
        "overdraw should 402, got: {overdraw}"
    );
    let withdrawal = covenant_compute_buyer::withdraw(
        &http,
        &covenant_compute_buyer::BuyerConfig {
            coordinator_url: base_url.clone(),
            poll_interval: Duration::from_millis(200),
            referral_code: None,
            rpc_url: None,
        },
        &buyer_identity,
        Uuid::new_v4(),
        remainder,
        &payer_pubkey,
    )
    .await
    .expect("withdraw the remainder");
    println!(
        "withdrawal debited: {} micro-usdc to {payer_pubkey} (memo: {})",
        withdrawal.amount_micro_usdc, withdrawal.memo
    );
    let signed_at_ms = epoch_ms();
    let signature =
        covenant_compute_protocol::sign_read(&buyer_identity, &balance_path, signed_at_ms)
            .expect("sign post-withdraw balance read");
    let drained: serde_json::Value = http
        .get(format!("{base_url}{balance_path}"))
        .header(
            covenant_compute_protocol::READ_SIGNED_AT_HEADER,
            signed_at_ms.to_string(),
        )
        .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, signature)
        .send()
        .await
        .expect("post-withdraw balance read")
        .json()
        .await
        .expect("decode post-withdraw balance");
    assert_eq!(drained["withdrawn_micro_usdc"], remainder);
    assert_eq!(drained["available_micro_usdc"], 0);

    println!("\n=== LIVE DEVNET DEPOSIT PROOF ===");
    println!("deposit tx: {deposit_signature}");
    println!("explorer: https://explorer.solana.com/tx/{deposit_signature}?cluster=devnet");
    println!("memo: {memo}");
    println!("credited: {DEPOSIT_MICRO_USDC} micro-usdc (re-claim credited nothing)");
    println!("pre-deposit job: 402; post-deposit job {job_id}: served and receipted");
    println!(
        "balance after spend: {remainder} micro-usdc available, then withdrawn to the \
         paying wallet — deposit in, work paid, remainder out, nothing trapped"
    );
}
