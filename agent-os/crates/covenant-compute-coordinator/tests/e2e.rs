//! Hermetic node<->coordinator end-to-end loop.
//!
//! Real axum coordinator server bound to an ephemeral loopback port, a
//! real `covenant-compute-node::Node` (echo executor) talking to it
//! through the real `HttpCoordinatorClient`, and a mock buyer POSTing a
//! signed job envelope with a plain `reqwest::Client`. Synthetic
//! ed25519 keys throughout (`LocalIdentity::generate`); no real funds,
//! no real GPU, no network egress — every request stays on 127.0.0.1.
//!
//! Covers the full happy path (build-notes-phase1-foundation.md §1.6's
//! handshake, steps 1-7) plus the three failure paths the brief calls
//! out: a bad envelope signature, a receipt that can't authorize a
//! release, and a deadline-expired refund. A "no capable operator"
//! refund is included too since `submit_job`'s admission-failed branch
//! is otherwise untested elsewhere.

use std::sync::Arc;
use std::time::Duration;

use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency, A2ATaskStatus};
use covenant_audit::{AuditKind, AuditLog, InMemoryAuditLog};
use covenant_compute_coordinator::{
    router, sweep_expired, sweep_unpaid, AuditReputationSource, CoordinatorConfig,
    CoordinatorState, MockPayout,
};
use covenant_compute_node::{
    Coordinator, EarningsLedger, EchoExecutor, HttpCoordinatorClient, InMemoryEarningsLedger, Node,
    NodeConfig, OllamaExecutor,
};
use covenant_compute_protocol::{
    sign_vault, vault_list_path, vault_open, vault_seal, vault_secret_path, vault_signing_path,
    BatchInclusionProof, CapabilityProfile, CapabilityRequirement, CapacityView, EscrowStatus,
    FederationEscrow, FundingSource, HardwareClass, JobEnvelopePayload, JobKind, JobMeter,
    PriceAsk, PriceUnit, RegisterRequest, SealedSecret, SettlementBatch, SettlementProof,
    SignedJobEnvelope, SignedWorkReceipt, VaultKey, WorkReceiptPayload, VAULT_SIGNATURE_HEADER,
    VAULT_SIGNED_AT_HEADER,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use uuid::Uuid;

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

async fn spawn_coordinator(state: CoordinatorState) -> String {
    let (url, _handle) = spawn_coordinator_abortable(state).await;
    url
}

/// A distinct, payable operator payout address per fixture seed —
/// registration refuses anything that doesn't decode to a 32-byte key,
/// and none of these tests wants the address to double as the
/// operator's own pubkey.
fn payout_addr(seed: u8) -> String {
    bs58::encode([seed; 32]).into_string()
}

/// The display-keyed sibling of [`payout_addr`], for fixtures that
/// register a whole roster in a loop.
fn payout_for(display: &str) -> String {
    let mut key = [0u8; 32];
    for (i, b) in display.bytes().take(32).enumerate() {
        key[i] = b;
    }
    bs58::encode(key).into_string()
}

async fn spawn_coordinator_abortable(
    state: CoordinatorState,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, router(state)).await.unwrap();
    });
    (format!("http://{addr}"), handle)
}

/// A killable TCP front door for the restart tests. Clients talk to a
/// stable address owned by this proxy — aborting an axum serve task
/// leaves its already-spawned connection tasks (and a client's pooled
/// keep-alive connection) alive, so the proxy is what makes "the
/// coordinator died" real: aborting it severs every connection
/// mid-flight, exactly like the process going down behind a deployed
/// service's stable URL. Pass `at` to re-bind the same address for the
/// next life.
async fn spawn_proxy(
    at: Option<std::net::SocketAddr>,
    upstream: std::net::SocketAddr,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let bind_addr = at.map(|a| a.to_string()).unwrap_or("127.0.0.1:0".into());
    let mut listener = None;
    for _ in 0..40 {
        match tokio::net::TcpListener::bind(&bind_addr).await {
            Ok(l) => {
                listener = Some(l);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    let listener = listener.expect("bind proxy address");
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let mut conns = tokio::task::JoinSet::new();
        loop {
            let Ok((mut inbound, _)) = listener.accept().await else {
                break;
            };
            conns.spawn(async move {
                let Ok(mut outbound) = tokio::net::TcpStream::connect(upstream).await else {
                    return;
                };
                let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
            });
        }
    });
    (addr, handle)
}

fn new_coordinator_state(long_poll_timeout: Duration) -> (CoordinatorState, Arc<MockPayout>) {
    let identity = LocalIdentity::generate("coordinator@e2e");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    let config = CoordinatorConfig {
        long_poll_timeout,
        default_funding_source: FundingSource::Organic,
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::new(identity, config, reputation, payout.clone(), audit);
    (state, payout)
}

fn cpu_profile(identity: &LocalIdentity, micro_usdc: u64) -> CapabilityProfile {
    CapabilityProfile {
        operator: identity.agent_id(),
        hardware: HardwareClass::CpuOnly,
        vram_gb: 0,
        models_served: vec!["any".into()],
        job_kinds: vec![JobKind::BatchJob],
        price: PriceAsk {
            unit: PriceUnit::PerJob,
            micro_usdc,
        },
        tee_capable: false,
        kind_prices: Vec::new(),
        kind_models: Vec::new(),
    }
}

fn signed_envelope(
    buyer: &LocalIdentity,
    job_id: Uuid,
    price_micro_usdc: u64,
    deadline_ms: u64,
    issued_at_ms: u64,
    idem_key: &str,
) -> SignedJobEnvelope {
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
        input: vec![Content::text("summarize: hermetic e2e loop")],
        price_micro_usdc,
        deadline_ms,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, idem_key),
        issued_at_ms,
        referral_code: None,
        stream: false,
    };
    SignedJobEnvelope::sign(payload, buyer).unwrap()
}

/// [`signed_envelope`] with a demand-side partner attribution signed
/// into the payload (C8): the buyer's own claim of who referred it.
fn referred_envelope(
    buyer: &LocalIdentity,
    job_id: Uuid,
    price_micro_usdc: u64,
    idem_key: &str,
    referral_code: &str,
) -> SignedJobEnvelope {
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
        input: vec![Content::text("summarize: hermetic e2e loop")],
        price_micro_usdc,
        deadline_ms: 30_000,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, idem_key),
        issued_at_ms: epoch_ms(),
        referral_code: Some(referral_code.into()),
        stream: false,
    };
    SignedJobEnvelope::sign(payload, buyer).unwrap()
}

/// A node advertising a single embedding model over `PerMillionTokens`
/// pricing — an embedding operator, matched only by an embedding ask.
fn embedding_profile(identity: &LocalIdentity, micro_usdc: u64) -> CapabilityProfile {
    CapabilityProfile {
        operator: identity.agent_id(),
        hardware: HardwareClass::CpuOnly,
        vram_gb: 0,
        models_served: vec!["nomic-embed-text".into()],
        job_kinds: vec![JobKind::Embedding],
        price: PriceAsk {
            unit: PriceUnit::PerMillionTokens,
            micro_usdc,
        },
        tee_capable: false,
        kind_prices: Vec::new(),
        kind_models: Vec::new(),
    }
}

fn embedding_envelope(
    buyer: &LocalIdentity,
    job_id: Uuid,
    price_micro_usdc: u64,
    issued_at_ms: u64,
) -> SignedJobEnvelope {
    let payload = JobEnvelopePayload {
        job_id,
        buyer: buyer.agent_id(),
        kind: JobKind::Embedding,
        capability_requirement: CapabilityRequirement {
            gpu_class: None,
            min_vram_gb: None,
            model_id: Some("nomic-embed-text".into()),
            kind: JobKind::Embedding,
            max_duration_secs: 30,
            min_reputation_bps: None,
        },
        input: vec![Content::text("embed this sentence")],
        price_micro_usdc,
        deadline_ms: 30_000,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "e2e-embed"),
        issued_at_ms,
        referral_code: None,
        stream: false,
    };
    SignedJobEnvelope::sign(payload, buyer).unwrap()
}

/// A stand-in Ollama `/api/embed` backend: one deterministic 4-dim
/// vector per input text, plus a prompt-token count so the receipt
/// meters like a real one.
async fn spawn_embed_backend() -> String {
    let app = axum::Router::new().route(
        "/api/embed",
        axum::routing::post(
            |axum::Json(body): axum::Json<serde_json::Value>| async move {
                let n = body["input"].as_array().map(Vec::len).unwrap_or(0);
                let embeddings: Vec<Vec<f32>> =
                    (0..n).map(|_| vec![0.1_f32, 0.2, 0.3, 0.4]).collect();
                axum::Json(serde_json::json!({
                    "model": body["model"],
                    "embeddings": embeddings,
                    "prompt_eval_count": 7,
                }))
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// The embedding loop end to end: a buyer's embedding job matches an
/// embedding-only operator, the node runs it against a real Ollama
/// `/api/embed` backend, and the vector settles like any other job —
/// verified receipt, released hold, payout, credited earnings — while
/// the buyer reads back a vector that hashes to what the operator signed.
#[tokio::test]
async fn an_embedding_job_runs_over_ollama_and_settles() {
    let backend = spawn_embed_backend().await;
    let (state, payout) = new_coordinator_state(Duration::from_secs(5));
    let state_handle = state.clone();
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@embed");
    let profile = embedding_profile(&operator_identity, 1_000);
    let coordinator_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        2,
    ));
    let register_req =
        RegisterRequest::sign(profile.clone(), payout_addr(3), &operator_identity).unwrap();
    assert!(
        coordinator_client
            .register(register_req)
            .await
            .unwrap()
            .accepted
    );

    let earnings = Arc::new(InMemoryEarningsLedger::new());
    let executor = Arc::new(
        OllamaExecutor::new(backend, None).require_models(["nomic-embed-text".to_string()]),
    );
    let node = Node::new(
        operator_identity,
        profile,
        coordinator_client,
        executor,
        earnings.clone(),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    );

    let buyer_identity = LocalIdentity::generate("buyer@embed");
    let job_id = Uuid::new_v4();
    let envelope = embedding_envelope(&buyer_identity, job_id, 1_000, epoch_ms());
    let http = reqwest::Client::new();
    let submit = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit.status(), reqwest::StatusCode::ACCEPTED);

    let outcome = node
        .run_once()
        .await
        .expect("run_once should succeed")
        .expect("the embedding job must have been offered to this operator");
    assert_eq!(outcome.job_id, job_id);
    assert!(
        outcome.error_message.is_none(),
        "{:?}",
        outcome.error_message
    );
    assert_eq!(outcome.receipt.receipt.status, A2ATaskStatus::Ok);
    outcome.receipt.verify().expect("receipt must verify");
    // Metered on input tokens; an embedding generates none.
    assert_eq!(outcome.receipt.receipt.meter.tokens_in, Some(7));
    assert_eq!(outcome.receipt.receipt.meter.tokens_out, None);

    // Settlement is identical to any other job.
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released
    );
    let payout_records = payout.records();
    assert_eq!(payout_records[0].amount_micro_usdc, 1_000);
    assert_eq!(earnings.unpaid_total_micro_usdc().await, 1_000);

    // The buyer reads back a real vector over its own signed poll, and it
    // hashes to exactly what the operator signed.
    let receipt_path = format!("/federation/jobs/{job_id}/receipt");
    let signed_at = epoch_ms();
    let sig =
        covenant_compute_protocol::sign_read(&buyer_identity, &receipt_path, signed_at).unwrap();
    let status: serde_json::Value = http
        .get(format!("{base_url}{receipt_path}"))
        .header(
            covenant_compute_protocol::READ_SIGNED_AT_HEADER,
            signed_at.to_string(),
        )
        .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, sig)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let output: Vec<Content> =
        serde_json::from_value(status["output"].clone()).expect("output present");
    assert_eq!(
        covenant_compute_protocol::output_hash_hex(&output),
        outcome.receipt.receipt.result_hash_hex,
        "the buyer's output hashes to what the operator signed"
    );
    let result = covenant_compute_protocol::parse_embedding_output(&output)
        .expect("the buyer receives an embedding, not a text completion");
    assert_eq!(result.model, "nomic-embed-text");
    assert_eq!(result.dimensions, 4);
    assert_eq!(result.embeddings, vec![vec![0.1, 0.2, 0.3, 0.4]]);
}

#[tokio::test]
async fn full_loop_matches_executes_releases_pays_and_audits() {
    let (state, payout) = new_coordinator_state(Duration::from_secs(5));
    let state_handle = state.clone();
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@e2e");
    let profile = cpu_profile(&operator_identity, 1_000);
    let coordinator_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        2,
    ));

    let register_req =
        RegisterRequest::sign(profile.clone(), payout_addr(2), &operator_identity).unwrap();
    let register_resp = coordinator_client.register(register_req).await.unwrap();
    assert!(register_resp.accepted);

    let node_audit = Arc::new(InMemoryAuditLog::new());
    let earnings = Arc::new(InMemoryEarningsLedger::new());
    let executor = Arc::new(EchoExecutor);
    let node = Node::new(
        operator_identity,
        profile,
        coordinator_client,
        executor,
        earnings.clone(),
        node_audit.clone(),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    );

    let buyer_identity = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let now_ms = epoch_ms();
    let envelope = signed_envelope(
        &buyer_identity,
        job_id,
        1_000,
        30_000,
        now_ms,
        "e2e-happy-path",
    );

    let http = reqwest::Client::new();
    let submit_resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit_resp.status(), reqwest::StatusCode::ACCEPTED);

    // matched -> JobOffer delivered via long-poll -> node admits (real
    // admit_job) -> executes -> signs WorkReceipt -> submits result.
    let outcome = node
        .run_once()
        .await
        .expect("run_once should succeed")
        .expect("the submitted job must have been offered to this operator");
    assert_eq!(outcome.job_id, job_id);
    assert!(outcome.error_message.is_none());
    assert_eq!(outcome.receipt.receipt.status, A2ATaskStatus::Ok);
    outcome.receipt.verify().expect("receipt must verify");

    // Coordinator verified the receipt and released the custodial hold.
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released
    );
    assert_eq!(
        state_handle.jobs().get(job_id).unwrap().phase,
        covenant_compute_coordinator::JobPhase::Completed
    );

    // Mock payout recorded the intended transfer for the held amount.
    let payout_records = payout.records();
    assert_eq!(payout_records.len(), 1);
    assert_eq!(payout_records[0].job_id, job_id);
    assert_eq!(payout_records[0].amount_micro_usdc, 1_000);
    assert_eq!(payout_records[0].payout_address, payout_addr(2));

    // The node's own earnings ledger credited the job.
    assert_eq!(earnings.unpaid_total_micro_usdc().await, 1_000);

    // The payout push landed on the journaled job record — the fact an
    // operator's paid/unpaid books are derived from.
    let recorded_payout = state_handle
        .jobs()
        .get(job_id)
        .unwrap()
        .payout
        .expect("a successful payout push must be pinned to the job record");
    assert_eq!(recorded_payout.amount_micro_usdc, 1_000);
    assert_eq!(
        recorded_payout.tx_signature, None,
        "MockPayout submits nothing on-chain"
    );

    // B4: the operator's own books are a signed read, same posture as
    // the buyer listings — amounts and payout confirmations for this
    // operator's work open only to its key.
    let operator_key = state_handle.jobs().get(job_id).unwrap().operator_pubkey_b58;
    let operator_jobs_path = format!("/federation/operators/{operator_key}/jobs");
    let unsigned_books = http
        .get(format!("{base_url}{operator_jobs_path}"))
        .send()
        .await
        .unwrap();
    assert_eq!(unsigned_books.status(), 401);
    let books_signed_at = epoch_ms();
    let books_sig =
        covenant_compute_protocol::sign_read(&node.identity, &operator_jobs_path, books_signed_at)
            .unwrap();
    let books: serde_json::Value = http
        .get(format!("{base_url}{operator_jobs_path}"))
        .header(
            covenant_compute_protocol::READ_SIGNED_AT_HEADER,
            books_signed_at.to_string(),
        )
        .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, books_sig)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = &books.as_array().expect("a row array")[0];
    assert_eq!(row["job_id"], job_id.to_string());
    assert_eq!(row["status"], "completed");
    assert_eq!(row["net_micro_usdc"], 1_000);
    assert_eq!(row["payout"]["amount_micro_usdc"], 1_000);
    assert_eq!(
        row["payout"]["tx_signature"],
        serde_json::Value::Null,
        "MockPayout reports the push with no on-chain signature"
    );

    // The buyer polls the coordinator and sees the verified receipt.
    // The poll is a signed read: the receipt carries the output, so a
    // bare job id must not open it.
    let receipt_path = format!("/federation/jobs/{job_id}/receipt");
    let poll_signed_at = epoch_ms();
    let poll_sig =
        covenant_compute_protocol::sign_read(&buyer_identity, &receipt_path, poll_signed_at)
            .unwrap();
    let status: serde_json::Value = http
        .get(format!("{base_url}{receipt_path}"))
        .header(
            covenant_compute_protocol::READ_SIGNED_AT_HEADER,
            poll_signed_at.to_string(),
        )
        .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, poll_sig)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["status"], "completed");
    let buyer_side_receipt: SignedWorkReceipt =
        serde_json::from_value(status["receipt"].clone()).expect("receipt present and well-formed");
    buyer_side_receipt
        .verify()
        .expect("buyer-visible receipt must independently verify");
    assert_eq!(buyer_side_receipt.receipt.job_id, job_id);

    // The poll also names the payout that honored this receipt, with
    // the memo the coordinator stamps on-chain — recomputable from the
    // receipt alone, so the buyer can check the coordinator isn't
    // inventing it.
    assert_eq!(status["payout"]["amount_micro_usdc"], 1_000);
    assert_eq!(
        status["payout"]["tx_signature"],
        serde_json::Value::Null,
        "MockPayout pushes no on-chain transfer"
    );
    assert_eq!(
        status["payout"]["memo"],
        buyer_side_receipt.payout_memo().as_str()
    );

    // The actual output came back with the receipt, and it hashes to
    // exactly what the operator signed — the buyer never has to trust
    // the coordinator's relay.
    let buyer_side_output: Vec<Content> =
        serde_json::from_value(status["output"].clone()).expect("output present");
    assert_eq!(
        buyer_side_output,
        vec![Content::text("summarize: hermetic e2e loop")],
        "echo executor returns the job input as output"
    );
    assert_eq!(
        covenant_compute_protocol::output_hash_hex(&buyer_side_output),
        buyer_side_receipt.receipt.result_hash_hex
    );

    // A4: the buyer's paid-for view lists the job, and the client
    // re-verifies the listed receipt locally.
    let rows = covenant_compute_buyer::list_verified_jobs(
        &http,
        &covenant_compute_buyer::BuyerConfig {
            coordinator_url: base_url.clone(),
            poll_interval: Duration::from_millis(100),
            referral_code: None,
            rpc_url: None,
        },
        &buyer_identity,
        10,
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].job_id, job_id);
    assert_eq!(rows[0].status, "completed");
    assert_eq!(rows[0].receipt_verified, Some(true));
    assert_eq!(rows[0].price_micro_usdc, 1_000);
    assert_eq!(
        rows[0].result_hash_hex.as_deref(),
        Some(buyer_side_receipt.receipt.result_hash_hex.as_str())
    );
    // History rows carry the payout block too, so one listing answers
    // "what ran, what did it cost, where did the money go".
    let listed_payout = rows[0].payout.as_ref().expect("payout on the paid row");
    assert_eq!(listed_payout.amount_micro_usdc, 1_000);
    assert_eq!(listed_payout.memo, buyer_side_receipt.payout_memo());

    // The same listing without a signed read is refused: envelopes
    // carry buyer inputs, and knowing a pubkey must not open them.
    let buyer_key = buyer_identity.agent_id().pubkey_base58();
    let unsigned = http
        .get(format!("{base_url}/federation/buyers/{buyer_key}/jobs"))
        .send()
        .await
        .unwrap();
    assert_eq!(unsigned.status(), 401);
    // Nor does a valid signature from a DIFFERENT key open this
    // buyer's history.
    let snoop = LocalIdentity::generate("snoop@e2e");
    let path = format!("/federation/buyers/{buyer_key}/jobs");
    let signed_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let sig = covenant_compute_protocol::sign_read(&snoop, &path, signed_at_ms).unwrap();
    let snooped = http
        .get(format!("{base_url}{path}"))
        .header(
            covenant_compute_protocol::READ_SIGNED_AT_HEADER,
            signed_at_ms.to_string(),
        )
        .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, sig)
        .send()
        .await
        .unwrap();
    assert_eq!(snooped.status(), 401);

    // The receipt poll is gated the same way: it returns the output,
    // so the job id alone (or someone else's key) must not open it.
    let unsigned_receipt = http
        .get(format!("{base_url}{receipt_path}"))
        .send()
        .await
        .unwrap();
    assert_eq!(unsigned_receipt.status(), 401);
    let snoop_receipt_sig =
        covenant_compute_protocol::sign_read(&snoop, &receipt_path, signed_at_ms).unwrap();
    let snooped_receipt = http
        .get(format!("{base_url}{receipt_path}"))
        .header(
            covenant_compute_protocol::READ_SIGNED_AT_HEADER,
            signed_at_ms.to_string(),
        )
        .header(
            covenant_compute_protocol::READ_SIGNATURE_HEADER,
            snoop_receipt_sig,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(snooped_receipt.status(), 401);

    // Both audit chains carry the compute-job rows and verify intact.
    let node_events = node_audit.recent(10).await.unwrap();
    assert!(node_events
        .iter()
        .any(|e| matches!(&e.kind, AuditKind::ComputeJobAdmitted { job_id: id, passed: true, .. } if *id == job_id)));
    assert!(node_events.iter().any(
        |e| matches!(&e.kind, AuditKind::ComputeJobCompleted { job_id: id, .. } if *id == job_id)
    ));
    assert!(node_audit.verify_integrity().await.unwrap().valid);

    let coordinator_events = state_handle.audit().recent(10).await.unwrap();
    assert!(coordinator_events.iter().any(
        |e| matches!(&e.kind, AuditKind::ComputeJobOffered { job_id: id, .. } if *id == job_id)
    ));
    assert!(coordinator_events.iter().any(
        |e| matches!(&e.kind, AuditKind::ComputeJobReleased { job_id: id, .. } if *id == job_id)
    ));
    assert!(coordinator_events.iter().any(|e| matches!(
        &e.kind,
        AuditKind::ComputePayoutPushed { job_id: id, amount_micro_usdc: 1_000, .. } if *id == job_id
    )));
    assert!(state_handle.audit().verify_integrity().await.unwrap().valid);
}

#[tokio::test]
async fn a_replayed_job_envelope_is_acknowledged_without_re_charging_or_re_dispatching() {
    // A signed job envelope's bytes reproduce verbatim, so a captured
    // one — or an honest retry of a submission whose response was
    // lost — can be re-POSTed after the job has already run and been
    // paid. It must resolve to an idempotent acknowledgement, never a
    // second hold, a second match, or a second payout.
    //
    // Short long-poll so the "no second offer was dispatched" check —
    // a node poll that must time out empty — resolves quickly.
    let (state, payout) = new_coordinator_state(Duration::from_millis(300));
    let state_handle = state.clone();
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@e2e");
    let profile = cpu_profile(&operator_identity, 1_000);
    let coordinator_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        2,
    ));
    let register_req =
        RegisterRequest::sign(profile.clone(), payout_addr(2), &operator_identity).unwrap();
    assert!(
        coordinator_client
            .register(register_req)
            .await
            .unwrap()
            .accepted
    );

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

    let buyer_identity = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let envelope = signed_envelope(
        &buyer_identity,
        job_id,
        1_000,
        30_000,
        epoch_ms(),
        "e2e-replay",
    );
    let http = reqwest::Client::new();

    let first = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), reqwest::StatusCode::ACCEPTED);

    let outcome = node
        .run_once()
        .await
        .unwrap()
        .expect("the first submission must be offered to the operator");
    assert_eq!(outcome.job_id, job_id);
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released
    );
    assert_eq!(payout.records().len(), 1);

    // Replay the exact same signed bytes, now that the job is paid.
    let replay = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert!(
        replay.status().is_success(),
        "a duplicate is an idempotent ack, not an error (got {})",
        replay.status()
    );
    let body: serde_json::Value = replay.json().await.unwrap();
    assert_eq!(body["job_id"], job_id.to_string());
    assert_eq!(
        body["status"], "completed",
        "the ack reports the job's real, already-terminal phase"
    );

    // The escrow was not reopened...
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released,
        "a replay must not flip a released hold back to Held"
    );
    // ...no second offer reached the operator (the poll times out
    // empty; a re-dispatch would have handed it a fresh job)...
    assert!(
        node.run_once().await.unwrap().is_none(),
        "the replay must not have produced a second offer"
    );
    // ...and the operator was paid exactly once.
    assert_eq!(
        payout.records().len(),
        1,
        "a replay must not trigger a second payout"
    );
    assert_eq!(
        state_handle
            .jobs()
            .by_buyer(&buyer_identity.agent_id().pubkey_base58())
            .len(),
        1,
        "the replay left the buyer with one job on the books, not two"
    );
}

/// The idempotent ack must outlive the envelope's own deadline: a
/// buyer that crashed mid-purchase retries after a restart, which is
/// exactly the buyer that arrives late. The deadline gate exists to
/// keep operators from being matched on jobs the sweep would kill —
/// it must not turn "already served and paid" into a 400.
#[tokio::test]
async fn a_replay_after_the_deadline_still_echoes_the_concluded_job() {
    let (state, payout) = new_coordinator_state(Duration::from_millis(300));
    let state_handle = state.clone();
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@e2e");
    let profile = cpu_profile(&operator_identity, 1_000);
    let coordinator_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        2,
    ));
    coordinator_client
        .register(
            RegisterRequest::sign(profile.clone(), payout_addr(2), &operator_identity).unwrap(),
        )
        .await
        .unwrap();
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

    let buyer_identity = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let envelope = signed_envelope(
        &buyer_identity,
        job_id,
        1_000,
        2_000,
        epoch_ms(),
        "e2e-late-replay",
    );
    let http = reqwest::Client::new();
    let first = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), reqwest::StatusCode::ACCEPTED);
    node.run_once()
        .await
        .unwrap()
        .expect("the job must be served inside its deadline");
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released
    );

    // Outlive the deadline, then replay the exact signed bytes. A
    // fresh envelope this late would be refused at admission; the
    // known job answers with its terminal phase instead.
    tokio::time::sleep(Duration::from_millis(2_300)).await;
    let replay = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(
        replay.status(),
        reqwest::StatusCode::OK,
        "a late replay is an idempotent ack, not a deadline refusal"
    );
    let body: serde_json::Value = replay.json().await.unwrap();
    assert_eq!(body["job_id"], job_id.to_string());
    assert_eq!(body["status"], "completed");
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released,
        "the late replay must not touch the settled hold"
    );
    assert_eq!(payout.records().len(), 1, "and never a second payout");
}

#[tokio::test]
async fn bad_envelope_signature_is_rejected_at_submit() {
    let (state, _payout) = new_coordinator_state(Duration::from_secs(2));
    let state_handle = state.clone();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@e2e");
    let register_req = RegisterRequest::sign(
        cpu_profile(&operator_identity, 1_000),
        payout_addr(1),
        &operator_identity,
    )
    .unwrap();
    let coordinator_client =
        HttpCoordinatorClient::with_config(base_url.clone(), Duration::from_secs(2), 1);
    coordinator_client.register(register_req).await.unwrap();

    let buyer_identity = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let now_ms = epoch_ms();
    let mut envelope = signed_envelope(
        &buyer_identity,
        job_id,
        1_000,
        30_000,
        now_ms,
        "e2e-bad-sig",
    );
    envelope
        .signature_b58
        .truncate(envelope.signature_b58.len() - 4);

    let http = reqwest::Client::new();
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // No hold, no job record, nothing offered.
    assert!(state_handle.jobs().get(job_id).is_none());
    let receipt_resp = http
        .get(format!("{base_url}/federation/jobs/{job_id}/receipt"))
        .send()
        .await
        .unwrap();
    assert_eq!(receipt_resp.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_job_whose_net_exceeds_the_per_payout_cap_is_refused_before_any_hold() {
    // A coordinator whose payout backend caps a single job's net at 500
    // micro-USDC. The cap is the backend's, not the buyer's — the buyer
    // learns it only by being refused, so the refusal must name it and
    // must leave no escrow hold behind. That is the whole point: a job
    // over the cap would match, complete, release the buyer's escrow, and
    // then strand at the payout push forever, so it must never be held.
    let identity = LocalIdentity::generate("coordinator@e2e");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::with_per_job_cap(500));
    let config = CoordinatorConfig {
        long_poll_timeout: Duration::from_secs(2),
        default_funding_source: FundingSource::Organic,
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::new(identity, config, reputation, payout, audit);
    let state_handle = state.clone();
    let base_url = spawn_coordinator(state).await;

    // An operator asking well under the cap, so the only thing that can
    // refuse the over-cap job is the cap gate — not a missing operator or
    // one priced above the offer.
    let operator_identity = LocalIdentity::generate("operator@e2e");
    let coordinator_client =
        HttpCoordinatorClient::with_config(base_url.clone(), Duration::from_secs(2), 1);
    coordinator_client
        .register(
            RegisterRequest::sign(
                cpu_profile(&operator_identity, 100),
                payout_addr(3),
                &operator_identity,
            )
            .unwrap(),
        )
        .await
        .unwrap();

    let buyer_identity = LocalIdentity::generate("buyer@e2e");
    let http = reqwest::Client::new();

    // Over the cap (the default fee is zero, so net == price): 400,
    // named, and nothing held or recorded.
    let over_id = Uuid::new_v4();
    let over = signed_envelope(
        &buyer_identity,
        over_id,
        1_000,
        30_000,
        epoch_ms(),
        "e2e-over-cap",
    );
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&over)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = resp.json().await.unwrap();
    let msg = body["error"].as_str().unwrap();
    assert!(msg.contains("per-payout cap"), "names the cap: {msg}");
    assert!(
        state_handle.escrow().status(over_id).await.is_err(),
        "no hold was created for the refused job"
    );
    assert!(state_handle.jobs().get(over_id).is_none());

    // At the cap: admitted, matched, and held — the boundary is
    // inclusive, since `pay` refuses only what exceeds it.
    let ok_id = Uuid::new_v4();
    let ok = signed_envelope(
        &buyer_identity,
        ok_id,
        500,
        30_000,
        epoch_ms(),
        "e2e-at-cap",
    );
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&ok)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(
        state_handle.escrow().status(ok_id).await.unwrap(),
        EscrowStatus::Held
    );
}

#[tokio::test]
async fn an_unpayable_payout_address_is_refused_at_register_and_admits_nothing() {
    let (state, _payout) = new_coordinator_state(Duration::from_secs(2));
    let base_url = spawn_coordinator(state).await;
    let http = reqwest::Client::new();
    let operator = LocalIdentity::generate("operator@e2e");

    // Honestly signed, so the address is the one defect — in both
    // classes: not base58 at all, and base58 of the wrong length.
    for bad in ["not-an-address!", &bs58::encode([7u8; 8]).into_string()] {
        let resp = http
            .post(format!("{base_url}/federation/operators/register"))
            .json(
                &RegisterRequest::sign(cpu_profile(&operator, 1_000), bad.to_string(), &operator)
                    .unwrap(),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST, "{bad}");
        let body: serde_json::Value = resp.json().await.unwrap();
        let msg = body["error"].as_str().unwrap();
        assert!(msg.contains("payout address"), "names the defect: {msg}");
    }

    // Neither refusal admitted anything.
    let view: CapacityView = reqwest::get(format!("{base_url}/federation/capacity"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(view.registered_operators, 0);

    // The refusal is per-request, not a ban: the same operator comes
    // back with a payable address and lands.
    let resp = http
        .post(format!("{base_url}/federation/operators/register"))
        .json(
            &RegisterRequest::sign(cpu_profile(&operator, 1_000), payout_addr(1), &operator)
                .unwrap(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let view: CapacityView = reqwest::get(format!("{base_url}/federation/capacity"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(view.registered_operators, 1);
}

#[tokio::test]
async fn malformed_inference_input_is_refused_at_submit_before_any_operator_sees_it() {
    let (state, _payout) = new_coordinator_state(Duration::from_secs(2));
    let state_handle = state.clone();
    let base_url = spawn_coordinator(state).await;

    // A live inference operator, so the only possible refusal reason
    // is the input itself — and the party a fault would land on if
    // this envelope ever got matched.
    let operator_identity = LocalIdentity::generate("operator@e2e");
    let mut profile = cpu_profile(&operator_identity, 1_000);
    profile.job_kinds = vec![JobKind::InferenceCall];
    let register_req = RegisterRequest::sign(profile, payout_addr(1), &operator_identity).unwrap();
    let coordinator_client =
        HttpCoordinatorClient::with_config(base_url.clone(), Duration::from_secs(2), 1);
    coordinator_client.register(register_req).await.unwrap();

    let buyer_identity = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let mut payload = signed_envelope(
        &buyer_identity,
        job_id,
        1_000,
        30_000,
        epoch_ms(),
        "e2e-malformed-knob",
    )
    .payload;
    payload.kind = JobKind::InferenceCall;
    payload.capability_requirement.kind = JobKind::InferenceCall;
    payload.input = vec![
        Content::text("hi"),
        Content::json(serde_json::json!({ "generation": { "temperature": 99 } })),
    ];
    let envelope = SignedJobEnvelope::sign(payload.clone(), &buyer_identity).unwrap();

    let http = reqwest::Client::new();
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body = resp.text().await.unwrap();
    assert!(body.contains("job input malformed"), "got: {body}");

    // Refused before any state: no hold, no record, no refund row an
    // operator could be faulted through.
    assert!(state_handle.jobs().get(job_id).is_none());

    // The same envelope with the knob in range clears the gate — the
    // refusal above was the input, not the kind.
    payload.job_id = Uuid::new_v4();
    payload.input = vec![
        Content::text("hi"),
        Content::json(serde_json::json!({ "generation": { "temperature": 0.7 } })),
    ];
    let well_formed = SignedJobEnvelope::sign(payload.clone(), &buyer_identity).unwrap();
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&well_formed)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "well-formed inference submit failed: {}",
        resp.status()
    );
    assert!(state_handle.jobs().get(payload.job_id).is_some());
}

#[tokio::test]
async fn an_envelope_already_past_its_deadline_is_refused_before_any_operator_sees_it() {
    let (state, _payout) = new_coordinator_state(Duration::from_secs(2));
    let state_handle = state.clone();
    let base_url = spawn_coordinator(state).await;

    // A live operator, so the only reason a submit can fail is the
    // envelope's own expiry — and the party the DeadlineExpired sweep
    // would fault if this job were ever matched.
    let operator_identity = LocalIdentity::generate("operator@e2e");
    let register_req = RegisterRequest::sign(
        cpu_profile(&operator_identity, 1_000),
        payout_addr(1),
        &operator_identity,
    )
    .unwrap();
    let coordinator_client =
        HttpCoordinatorClient::with_config(base_url.clone(), Duration::from_secs(2), 1);
    coordinator_client.register(register_req).await.unwrap();

    // issued two minutes ago with a one-second deadline: expired long
    // before it reaches the coordinator.
    let buyer_identity = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let stale = signed_envelope(
        &buyer_identity,
        job_id,
        1_000,
        1_000,
        epoch_ms() - 120_000,
        "e2e-expired",
    );

    let http = reqwest::Client::new();
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&stale)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body = resp.text().await.unwrap();
    assert!(body.contains("already past its deadline"), "got: {body}");

    // Refused before any state: no hold, no record, no DeadlineExpired
    // refund row the operator could be faulted through.
    assert!(state_handle.jobs().get(job_id).is_none());
    assert!(state_handle.escrow().status(job_id).await.is_err());

    // The same buyer with an honest live deadline clears the gate — the
    // refusal was the expiry, not the buyer or the price.
    let fresh = signed_envelope(
        &buyer_identity,
        Uuid::new_v4(),
        1_000,
        30_000,
        epoch_ms(),
        "e2e-live",
    );
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&fresh)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
}

#[tokio::test]
async fn a_receipt_not_attributable_to_the_assigned_operator_cannot_release_escrow() {
    let (state, payout) = new_coordinator_state(Duration::from_secs(2));
    let state_handle = state.clone();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@e2e");
    let register_req = RegisterRequest::sign(
        cpu_profile(&operator_identity, 1_000),
        payout_addr(1),
        &operator_identity,
    )
    .unwrap();
    let coordinator_client =
        HttpCoordinatorClient::with_config(base_url.clone(), Duration::from_secs(2), 1);
    coordinator_client.register(register_req).await.unwrap();

    let buyer_identity = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let now_ms = epoch_ms();
    let envelope = signed_envelope(
        &buyer_identity,
        job_id,
        1_000,
        30_000,
        now_ms,
        "e2e-forged-receipt",
    );

    let http = reqwest::Client::new();
    let submit_resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit_resp.status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Held
    );

    // A fully self-consistent, validly signed receipt — but from an
    // impostor identity, not the operator this job was actually
    // assigned to. `SignedWorkReceipt::verify()` alone would pass.
    let impostor = LocalIdentity::generate("impostor@e2e");
    let forged_receipt = SignedWorkReceipt::sign(
        WorkReceiptPayload {
            job_id,
            operator: impostor.agent_id(),
            job_hash_hex: "aa".repeat(32),
            result_hash_hex: "bb".repeat(32),
            meter: JobMeter {
                wall_ms: 1,
                tokens_in: None,
                tokens_out: None,
                gpu_seconds: None,
                finish_reason: None,
            },
            price_micro_usdc: 1_000,
            status: A2ATaskStatus::Ok,
            executed_at_ms: now_ms,
            node_audit_root_hex: "cc".repeat(32),
        },
        &impostor,
    )
    .unwrap();
    forged_receipt
        .verify()
        .expect("the forged receipt is internally self-consistent");

    let result_resp = http
        .post(format!("{base_url}/federation/jobs/{job_id}/result"))
        .json(&covenant_compute_protocol::JobResultMessage {
            receipt: forged_receipt,
            output: vec![],
        })
        .send()
        .await
        .unwrap();
    assert_eq!(result_resp.status(), reqwest::StatusCode::BAD_REQUEST);

    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Held,
        "an unattributable receipt must never release the hold"
    );
    assert!(payout.records().is_empty());
}

#[tokio::test]
async fn output_that_does_not_hash_to_the_signed_receipt_cannot_release_escrow() {
    let (state, payout) = new_coordinator_state(Duration::from_secs(2));
    let state_handle = state.clone();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@e2e");
    let register_req = RegisterRequest::sign(
        cpu_profile(&operator_identity, 1_000),
        payout_addr(1),
        &operator_identity,
    )
    .unwrap();
    let coordinator_client =
        HttpCoordinatorClient::with_config(base_url.clone(), Duration::from_secs(2), 1);
    coordinator_client.register(register_req).await.unwrap();

    let buyer_identity = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let now_ms = epoch_ms();
    let envelope = signed_envelope(
        &buyer_identity,
        job_id,
        1_000,
        30_000,
        now_ms,
        "e2e-tampered-output",
    );

    let http = reqwest::Client::new();
    let submit_resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit_resp.status(), reqwest::StatusCode::ACCEPTED);

    // Signed by the correctly-assigned operator — attribution passes —
    // but the message carries different bytes than the receipt's
    // result_hash_hex commits to.
    let honest_output = vec![Content::text("honest output")];
    let receipt = SignedWorkReceipt::sign(
        WorkReceiptPayload {
            job_id,
            operator: operator_identity.agent_id(),
            job_hash_hex: "aa".repeat(32),
            result_hash_hex: covenant_compute_protocol::output_hash_hex(&honest_output),
            meter: JobMeter {
                wall_ms: 1,
                tokens_in: None,
                tokens_out: None,
                gpu_seconds: None,
                finish_reason: None,
            },
            price_micro_usdc: 1_000,
            status: A2ATaskStatus::Ok,
            executed_at_ms: now_ms,
            node_audit_root_hex: "cc".repeat(32),
        },
        &operator_identity,
    )
    .unwrap();

    // The same valid receipt posted to a different job's result path is
    // refused before the job is even looked up — the path and the
    // receipt have to name the same job, so a receipt can't be replayed
    // onto another hold.
    let misrouted = http
        .post(format!(
            "{base_url}/federation/jobs/{}/result",
            Uuid::new_v4()
        ))
        .json(&covenant_compute_protocol::JobResultMessage {
            receipt: receipt.clone(),
            output: honest_output.clone(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(misrouted.status(), reqwest::StatusCode::BAD_REQUEST);

    // A receipt whose price was inflated after signing no longer
    // verifies: the price is inside the signed payload, so a relay
    // that bumps its own pay is refused before attribution or
    // settlement even look at the job.
    let mut inflated = receipt.clone();
    inflated.receipt.price_micro_usdc = 2_000;
    let resp = http
        .post(format!("{base_url}/federation/jobs/{job_id}/result"))
        .json(&covenant_compute_protocol::JobResultMessage {
            receipt: inflated,
            output: honest_output.clone(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body = resp.text().await.unwrap();
    assert!(body.contains("receipt does not verify"), "got: {body}");

    let result_resp = http
        .post(format!("{base_url}/federation/jobs/{job_id}/result"))
        .json(&covenant_compute_protocol::JobResultMessage {
            receipt,
            output: vec![Content::text("tampered output")],
        })
        .send()
        .await
        .unwrap();
    assert_eq!(result_resp.status(), reqwest::StatusCode::BAD_REQUEST);

    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Held,
        "unverifiable output must never release the hold"
    );
    assert!(payout.records().is_empty());
    assert!(state_handle.jobs().get(job_id).unwrap().output.is_none());
}

/// A relay that inflates the signed price is refused before any money
/// moves: the price lives inside the signed payload, so the tampered
/// envelope no longer verifies — no hold, no record, nothing offered.
#[tokio::test]
async fn a_tampered_envelope_is_refused_before_any_money_moves() {
    let (state, _payout) = new_coordinator_state(Duration::from_secs(2));
    let state_handle = state.clone();
    let base_url = spawn_coordinator(state).await;

    // A live operator, so the honest control submit below can only
    // fail for the reason under test.
    let operator_identity = LocalIdentity::generate("operator@e2e");
    let register_req = RegisterRequest::sign(
        cpu_profile(&operator_identity, 1_000),
        payout_addr(1),
        &operator_identity,
    )
    .unwrap();
    HttpCoordinatorClient::with_config(base_url.clone(), Duration::from_secs(2), 1)
        .register(register_req)
        .await
        .unwrap();

    let buyer_identity = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let mut envelope = signed_envelope(
        &buyer_identity,
        job_id,
        1_000,
        30_000,
        epoch_ms(),
        "e2e-tampered-envelope",
    );
    envelope.payload.price_micro_usdc = 2_000;

    let http = reqwest::Client::new();
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body = resp.text().await.unwrap();
    assert!(body.contains("envelope does not verify"), "got: {body}");

    // Refused before any state: no hold, no record, no operator offer.
    assert!(state_handle.jobs().get(job_id).is_none());
    assert!(state_handle.escrow().status(job_id).await.is_err());

    // The buyer's honest envelope for the same job clears the gate —
    // the refusal was the tamper, not the job.
    let honest = signed_envelope(
        &buyer_identity,
        job_id,
        1_000,
        30_000,
        epoch_ms(),
        "e2e-tampered-envelope",
    );
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&honest)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
}

#[tokio::test]
async fn a_job_past_its_deadline_with_no_receipt_is_refunded_by_the_sweep() {
    let (state, _payout) = new_coordinator_state(Duration::from_secs(2));
    let state_handle = state.clone();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@e2e");
    let register_req = RegisterRequest::sign(
        cpu_profile(&operator_identity, 1_000),
        payout_addr(1),
        &operator_identity,
    )
    .unwrap();
    let coordinator_client =
        HttpCoordinatorClient::with_config(base_url.clone(), Duration::from_secs(2), 1);
    coordinator_client.register(register_req).await.unwrap();

    let buyer_identity = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    // Admitted live — a short deadline still in the future at submit —
    // then swept once the coordinator clock is past it. This is the
    // real deadline-refund path: a job that expires waiting on a
    // receipt that never comes, not one submitted already stale (that
    // is refused at admission, see the test above).
    let issued_at_ms = epoch_ms();
    let deadline_ms = 1_000;
    let envelope = signed_envelope(
        &buyer_identity,
        job_id,
        1_000,
        deadline_ms,
        issued_at_ms,
        "e2e-deadline",
    );

    let http = reqwest::Client::new();
    let submit_resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit_resp.status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Held
    );

    let refunded = sweep_expired(&state_handle, issued_at_ms + deadline_ms + 1).await;
    assert!(refunded.contains(&job_id));
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Refunded
    );
    let record = state_handle.jobs().get(job_id).unwrap();
    assert_eq!(
        record.phase,
        covenant_compute_coordinator::JobPhase::Refunded
    );
    assert_eq!(
        record.refund_reason,
        Some(covenant_compute_protocol::RefundReason::DeadlineExpired),
        "the record says why the money came back, not just that it did"
    );

    let events = state_handle.audit().recent(10).await.unwrap();
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        AuditKind::ComputeJobRefunded { job_id: id, reason, .. } if *id == job_id && reason == "deadline_expired"
    )));
}

#[tokio::test]
async fn a_result_delivered_after_the_deadline_refunds_instead_of_paying() {
    // The sweep is not the only thing that must honor the deadline: a
    // result that lands after it — but before the next 10s sweep tick —
    // must still refund, not pay, or whether the operator is paid turns
    // on sweep timing. No periodic sweep runs here (the router alone is
    // spawned), so the refund below is the result handler's own doing.
    let (state, payout) = new_coordinator_state(Duration::from_secs(2));
    let state_handle = state.clone();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@e2e");
    let register_req = RegisterRequest::sign(
        cpu_profile(&operator_identity, 1_000),
        payout_addr(1),
        &operator_identity,
    )
    .unwrap();
    let coordinator_client =
        HttpCoordinatorClient::with_config(base_url.clone(), Duration::from_secs(2), 1);
    coordinator_client.register(register_req).await.unwrap();

    // Admitted live with a short deadline, then left to expire before a
    // result is returned.
    let buyer_identity = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let envelope = signed_envelope(&buyer_identity, job_id, 1_000, 500, epoch_ms(), "e2e-late");
    let http = reqwest::Client::new();
    let submit_resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit_resp.status(), reqwest::StatusCode::ACCEPTED);

    tokio::time::sleep(Duration::from_millis(900)).await;

    // A fully valid, correctly-attributed Ok receipt — the only thing
    // wrong with it is that it arrives past the deadline.
    let output = vec![Content::text("late but correct")];
    let receipt = SignedWorkReceipt::sign(
        WorkReceiptPayload {
            job_id,
            operator: operator_identity.agent_id(),
            job_hash_hex: "aa".repeat(32),
            result_hash_hex: covenant_compute_protocol::output_hash_hex(&output),
            meter: JobMeter {
                wall_ms: 1,
                tokens_in: None,
                tokens_out: None,
                gpu_seconds: None,
                finish_reason: None,
            },
            price_micro_usdc: 1_000,
            status: A2ATaskStatus::Ok,
            executed_at_ms: epoch_ms(),
            node_audit_root_hex: "cc".repeat(32),
        },
        &operator_identity,
    )
    .unwrap();

    let result_resp = http
        .post(format!("{base_url}/federation/jobs/{job_id}/result"))
        .json(&covenant_compute_protocol::JobResultMessage { receipt, output })
        .send()
        .await
        .unwrap();
    assert_eq!(result_resp.status(), reqwest::StatusCode::OK);
    let ack: covenant_compute_protocol::JobResultAck = result_resp.json().await.unwrap();
    assert_eq!(
        ack.settled,
        covenant_compute_protocol::ResultSettlement::Refunded,
        "the ack must tell the operator its delivery settled as a refund"
    );

    // Refunded, not released — and the operator was paid nothing.
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Refunded,
        "a past-deadline result must refund the buyer, never release the hold"
    );
    let record = state_handle.jobs().get(job_id).unwrap();
    assert_eq!(
        record.phase,
        covenant_compute_coordinator::JobPhase::Refunded
    );
    assert_eq!(
        record.refund_reason,
        Some(covenant_compute_protocol::RefundReason::DeadlineExpired)
    );
    assert!(
        record.receipt.is_some(),
        "the late receipt is kept as evidence even though it paid nothing"
    );
    assert!(
        payout.records().is_empty(),
        "no payout may fire for a result that missed the deadline"
    );
    let events = state_handle.audit().recent(10).await.unwrap();
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        AuditKind::ComputeJobRefunded { job_id: id, reason, .. }
            if *id == job_id && reason == "deadline_expired"
    )));
}

/// The result handler's deadline branch carries the same `AlreadySettled`
/// ambiguity `cancel_job` resolves: a within-deadline `Ok` result can RELEASE
/// the hold and then die before its record write lands (the crash window
/// `submit_result`'s refill heals), and the operator's outbox then redelivers
/// after the deadline. That redelivery must recover the operator's payout, not
/// refund a job the buyer was already charged for — the deadline refund is for
/// work that never settled, not for a paid result that merely arrived late.
#[tokio::test]
async fn a_paid_result_redelivered_after_the_deadline_recovers_the_payout_not_a_refund() {
    let (state, payout) = new_coordinator_state(Duration::from_secs(2));
    let state_handle = state.clone();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@e2e");
    let register_req = RegisterRequest::sign(
        cpu_profile(&operator_identity, 1_000),
        payout_addr(1),
        &operator_identity,
    )
    .unwrap();
    let coordinator_client =
        HttpCoordinatorClient::with_config(base_url.clone(), Duration::from_secs(2), 1);
    coordinator_client.register(register_req).await.unwrap();

    let buyer_identity = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let envelope = signed_envelope(
        &buyer_identity,
        job_id,
        1_000,
        500,
        epoch_ms(),
        "e2e-late-paid",
    );
    let http = reqwest::Client::new();
    let submit_resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit_resp.status(), reqwest::StatusCode::ACCEPTED);

    // A fully valid Ok receipt for the delivered work.
    let output = vec![Content::text("paid, then redelivered late")];
    let receipt = SignedWorkReceipt::sign(
        WorkReceiptPayload {
            job_id,
            operator: operator_identity.agent_id(),
            job_hash_hex: "aa".repeat(32),
            result_hash_hex: covenant_compute_protocol::output_hash_hex(&output),
            meter: JobMeter {
                wall_ms: 1,
                tokens_in: None,
                tokens_out: None,
                gpu_seconds: None,
                finish_reason: None,
            },
            price_micro_usdc: 1_000,
            status: A2ATaskStatus::Ok,
            executed_at_ms: epoch_ms(),
            node_audit_root_hex: "cc".repeat(32),
        },
        &operator_identity,
    )
    .unwrap();

    // The within-deadline result released the hold, then the process died
    // before the record write: escrow reads `Released` while the record stays
    // receipt-less — exactly the crash window boot reconcile heals to a
    // receipt-less `Completed`.
    state_handle
        .escrow()
        .release(job_id, &receipt)
        .await
        .unwrap();
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released,
    );
    assert!(
        state_handle.jobs().get(job_id).unwrap().receipt.is_none(),
        "the record has no receipt yet — the write died in the crash window",
    );

    // The deadline lapses, then the operator's outbox redelivers the receipt.
    tokio::time::sleep(Duration::from_millis(900)).await;

    let result_resp = http
        .post(format!("{base_url}/federation/jobs/{job_id}/result"))
        .json(&covenant_compute_protocol::JobResultMessage { receipt, output })
        .send()
        .await
        .unwrap();
    assert_eq!(result_resp.status(), reqwest::StatusCode::OK);
    let ack: covenant_compute_protocol::JobResultAck = result_resp.json().await.unwrap();
    assert_eq!(
        ack.settled,
        covenant_compute_protocol::ResultSettlement::Released,
        "a redelivery of a paid result must settle as released, not refunded",
    );

    // The hold stays released — the buyer keeps their charge and the operator
    // keeps its pay — the record heals to `Completed` with its receipt, and the
    // payout the crash window dropped is pushed.
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released,
        "a paid redelivery must never flip the hold back to refunded",
    );
    let record = state_handle.jobs().get(job_id).unwrap();
    assert_eq!(
        record.phase,
        covenant_compute_coordinator::JobPhase::Completed
    );
    assert!(record.receipt.is_some());
    assert_eq!(
        record.refund_reason, None,
        "a paid job carries no refund reason",
    );
    assert!(
        !payout.records().is_empty(),
        "the operator is paid the recovered result, not left stranded",
    );
    let events = state_handle.audit().recent(10).await.unwrap();
    assert!(
        !events.iter().any(|e| matches!(
            &e.kind,
            AuditKind::ComputeJobRefunded { job_id: id, reason, .. }
                if *id == job_id && reason == "deadline_expired"
        )),
        "no deadline_expired refund is recorded for a job the operator was paid for",
    );
}

/// The non-Ok branch carries the ambiguity too: a prior `Ok` result can have
/// RELEASED the hold before a (non-conforming) non-Ok receipt lands for the
/// same job. That must report the charge, never a phantom refund of money the
/// operator was paid — the settlement path refuses the unpayable receipt as a
/// conflict, and the hold stays released.
#[tokio::test]
async fn a_non_ok_receipt_on_a_released_hold_conflicts_rather_than_phantom_refunding() {
    let (state, _payout) = new_coordinator_state(Duration::from_secs(30));
    let state_handle = state.clone();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@e2e");
    let register_req = RegisterRequest::sign(
        cpu_profile(&operator_identity, 1_000),
        payout_addr(1),
        &operator_identity,
    )
    .unwrap();
    let coordinator_client =
        HttpCoordinatorClient::with_config(base_url.clone(), Duration::from_secs(2), 1);
    coordinator_client.register(register_req).await.unwrap();

    let buyer_identity = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let envelope = signed_envelope(
        &buyer_identity,
        job_id,
        1_000,
        30_000,
        epoch_ms(),
        "e2e-nonok-released",
    );
    let http = reqwest::Client::new();
    let submit_resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit_resp.status(), reqwest::StatusCode::ACCEPTED);

    // A prior Ok result released the hold; its record write was lost.
    let ok_output = vec![Content::text("the paid answer")];
    let ok_receipt = SignedWorkReceipt::sign(
        WorkReceiptPayload {
            job_id,
            operator: operator_identity.agent_id(),
            job_hash_hex: "aa".repeat(32),
            result_hash_hex: covenant_compute_protocol::output_hash_hex(&ok_output),
            meter: JobMeter {
                wall_ms: 1,
                tokens_in: None,
                tokens_out: None,
                gpu_seconds: None,
                finish_reason: None,
            },
            price_micro_usdc: 1_000,
            status: A2ATaskStatus::Ok,
            executed_at_ms: epoch_ms(),
            node_audit_root_hex: "cc".repeat(32),
        },
        &operator_identity,
    )
    .unwrap();
    state_handle
        .escrow()
        .release(job_id, &ok_receipt)
        .await
        .unwrap();
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released,
    );

    // A non-Ok receipt then arrives for the same, already-paid job.
    let fail_output = vec![Content::text("execution failed")];
    let fail_receipt = SignedWorkReceipt::sign(
        WorkReceiptPayload {
            job_id,
            operator: operator_identity.agent_id(),
            job_hash_hex: "aa".repeat(32),
            result_hash_hex: covenant_compute_protocol::output_hash_hex(&fail_output),
            meter: JobMeter {
                wall_ms: 1,
                tokens_in: None,
                tokens_out: None,
                gpu_seconds: None,
                finish_reason: None,
            },
            price_micro_usdc: 1_000,
            status: A2ATaskStatus::Error,
            executed_at_ms: epoch_ms(),
            node_audit_root_hex: "cc".repeat(32),
        },
        &operator_identity,
    )
    .unwrap();
    let result_resp = http
        .post(format!("{base_url}/federation/jobs/{job_id}/result"))
        .json(&covenant_compute_protocol::JobResultMessage {
            receipt: fail_receipt,
            output: fail_output,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(
        result_resp.status(),
        reqwest::StatusCode::CONFLICT,
        "a non-Ok receipt for a hold the operator was already paid from is a conflict, not a refund",
    );

    // The hold stays released and no phantom execution_failed refund is booked.
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released,
    );
    let events = state_handle.audit().recent(10).await.unwrap();
    assert!(
        !events.iter().any(|e| matches!(
            &e.kind,
            AuditKind::ComputeJobRefunded { job_id: id, reason, .. }
                if *id == job_id && reason == "execution_failed"
        )),
        "no execution_failed refund is recorded for a job the operator was paid for",
    );
}

#[tokio::test]
async fn a_node_does_not_execute_a_job_that_expired_in_its_queue() {
    use covenant_compute_node::{ExecutionOutcome, ExecutorError, JobExecutor};
    use covenant_compute_protocol::JobEnvelopePayload;
    use std::sync::atomic::{AtomicBool, Ordering};

    // Flips only if the executor is actually asked to run. The buyer's
    // deadline is absolute, so a job dequeued after it has zero budget
    // left — the node must refuse to spend compute on it, not hand the
    // executor a fresh full deadline.
    struct SpyExecutor(Arc<AtomicBool>);

    #[async_trait::async_trait]
    impl JobExecutor for SpyExecutor {
        async fn execute(
            &self,
            _job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<ExecutionOutcome, ExecutorError> {
            self.0.store(true, Ordering::SeqCst);
            Ok(ExecutionOutcome {
                output: vec![Content::text("should never run")],
                wall_ms: 1,
                tokens_in: None,
                tokens_out: None,
                finish_reason: None,
            })
        }
    }

    let (state, payout) = new_coordinator_state(Duration::from_secs(5));
    let state_handle = state.clone();
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@e2e");
    let profile = cpu_profile(&operator_identity, 1_000);
    let coordinator_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        2,
    ));
    let register_req =
        RegisterRequest::sign(profile.clone(), payout_addr(1), &operator_identity).unwrap();
    coordinator_client.register(register_req).await.unwrap();

    let executed = Arc::new(AtomicBool::new(false));
    let node = Node::new(
        operator_identity,
        profile,
        coordinator_client,
        Arc::new(SpyExecutor(executed.clone())),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    );

    // Admitted live (300ms deadline), then it sits long enough to
    // expire before the node dequeues it — issued_at stays fresh within
    // the node's admission window, so the offer is delivered normally.
    let buyer_identity = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let envelope = signed_envelope(
        &buyer_identity,
        job_id,
        1_000,
        300,
        epoch_ms(),
        "e2e-queued",
    );
    let http = reqwest::Client::new();
    let submit_resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit_resp.status(), reqwest::StatusCode::ACCEPTED);

    tokio::time::sleep(Duration::from_millis(450)).await;

    // The node dequeues the now-expired offer: it admits and accepts,
    // but the remaining budget is zero, so the executor is never run.
    let outcome = node
        .run_once()
        .await
        .expect("run_once succeeds")
        .expect("the offer was delivered");
    assert_eq!(outcome.job_id, job_id);
    assert!(
        !executed.load(Ordering::SeqCst),
        "the executor must not run for a job already past its deadline"
    );
    assert_eq!(
        outcome.receipt.receipt.status,
        A2ATaskStatus::Error,
        "the node reports the expired job as a timeout, not a served result"
    );

    // And the coordinator refunds it — the operator earns nothing for a
    // job it correctly declined to compute.
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Refunded
    );
    assert!(payout.records().is_empty());
}

#[tokio::test]
async fn a_failed_job_refunds_the_buyer_and_pays_the_operator_nothing() {
    use covenant_compute_buyer::{dispatch_and_verify, BuyerConfig, BuyerError, JobRequest};
    use covenant_compute_node::{ExecutionOutcome, ExecutorError, JobExecutor};
    use covenant_compute_protocol::JobEnvelopePayload;

    struct FailingExecutor;

    #[async_trait::async_trait]
    impl JobExecutor for FailingExecutor {
        async fn execute(
            &self,
            _job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<ExecutionOutcome, ExecutorError> {
            Err(ExecutorError::Failed("model backend unavailable".into()))
        }
    }

    let (state, payout) = new_coordinator_state(Duration::from_secs(2));
    let state_handle = state.clone();
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@e2e");
    let profile = CapabilityProfile {
        job_kinds: vec![JobKind::InferenceCall],
        ..cpu_profile(&operator_identity, 1_000)
    };
    let coordinator_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        2,
    ));
    let register_req =
        RegisterRequest::sign(profile.clone(), payout_addr(18), &operator_identity).unwrap();
    coordinator_client.register(register_req).await.unwrap();

    let earnings = Arc::new(InMemoryEarningsLedger::new());
    let node = Node::new(
        operator_identity,
        profile,
        coordinator_client,
        Arc::new(FailingExecutor),
        earnings.clone(),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    );
    let node_task = tokio::spawn(async move {
        loop {
            if let Some(outcome) = node.run_once().await.expect("run_once") {
                return outcome;
            }
        }
    });

    // The REAL buyer client dispatches: it must come back refused, not
    // hand the caller a failure receipt as if it were a served result.
    let buyer_identity = LocalIdentity::generate("buyer@e2e");
    let http = reqwest::Client::new();
    let err = dispatch_and_verify(
        &http,
        &BuyerConfig {
            coordinator_url: base_url.clone(),
            poll_interval: Duration::from_millis(100),
            referral_code: None,
            rpc_url: None,
        },
        &buyer_identity,
        JobRequest {
            kind: JobKind::InferenceCall,
            input: vec![Content::text("summarize: doomed job")],
            model: None,
            gpu_class: None,
            min_vram_gb: None,
            min_reputation_bps: None,
            price_micro_usdc: 1_000,
            deadline_ms: 30_000,
        },
    )
    .await
    .expect_err("a failed job must not verify as served");
    let (job_id, failure_detail) = match err {
        BuyerError::NotServed {
            job_id,
            ref status,
            ref reason,
            ref detail,
        } if status == "failed" && reason.as_deref() == Some("execution_failed") => {
            (job_id, detail.clone())
        }
        other => panic!("expected NotServed(failed, execution_failed), got {other:?}"),
    };
    // The buyer learns *why* it failed: the operator's own cause, carried
    // as the failure receipt's signed output and re-verified locally —
    // not just the coordinator's bare "execution_failed".
    let detail = failure_detail.expect("a failed job surfaces the operator's signed cause");
    assert!(
        detail.contains("model backend unavailable"),
        "detail names the executor's reported cause: {detail}"
    );

    // The node's submission itself succeeded and carries the error.
    let outcome = node_task.await.unwrap();
    assert_eq!(outcome.job_id, job_id);
    assert_eq!(outcome.receipt.receipt.status, A2ATaskStatus::Error);
    assert!(outcome.error_message.is_some());

    // Money: hold refunded, nothing released, nothing paid, nothing earned.
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Refunded
    );
    assert!(payout.records().is_empty());
    assert_eq!(earnings.unpaid_total_micro_usdc().await, 0);

    // The failure receipt is kept as the operator's own signed evidence.
    let record = state_handle.jobs().get(job_id).unwrap();
    assert_eq!(record.phase, covenant_compute_coordinator::JobPhase::Failed);
    assert_eq!(
        record.refund_reason,
        Some(covenant_compute_protocol::RefundReason::ExecutionFailed)
    );
    let operator_pubkey = record.operator_pubkey_b58.clone();
    // The failure receipt's output is the operator's signed cause — hashed
    // into the receipt, so the coordinator only accepted it because it
    // matched — not the empty output a failed job used to carry.
    let failure_output = record.output.clone().expect("failure output retained");
    let output_text: String = failure_output
        .iter()
        .filter_map(|c| match c {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        output_text.contains("model backend unavailable"),
        "failure output carries the executor's cause: {output_text}"
    );
    let kept = record.receipt.expect("failure receipt retained");
    assert_eq!(kept.receipt.status, A2ATaskStatus::Error);
    kept.verify().unwrap();

    // The refund row is attributed to the operator whose executor
    // failed, and reputation counts it as a concluded fault: the score
    // drops below an unknown operator's neutral prior.
    let events = state_handle.audit().recent(10).await.unwrap();
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        AuditKind::ComputeJobRefunded { job_id: id, reason, operator_pubkey_b58: Some(op) } if *id == job_id && reason == "execution_failed" && *op == operator_pubkey
    )));
    assert!(!events
        .iter()
        .any(|e| matches!(&e.kind, AuditKind::ComputeJobReleased { .. })));

    let faulted: serde_json::Value = http
        .get(format!(
            "{base_url}/federation/operators/{operator_pubkey}/reputation"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(faulted["released"], 0);
    assert_eq!(faulted["faults"], 1);
    let unknown: serde_json::Value = http
        .get(format!(
            "{base_url}/federation/operators/never-seen/reputation"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(unknown["faults"], 0);
    assert!(
        faulted["score_bps"].as_u64() < unknown["score_bps"].as_u64(),
        "a proven failure record must rank below no record at all"
    );

    // Standing rides the same read: the faulted operator is still
    // registered, fresh, and above this deployment's (disabled) floors
    // — a fault dents the score, not matchability. The stranger is
    // simply not in the directory.
    assert_eq!(faulted["registered"], true);
    assert_eq!(faulted["status"], "online");
    assert_eq!(faulted["live"], true);
    assert_eq!(faulted["matchable"], true);
    assert!(faulted["seen_ms_ago"].as_u64().is_some());
    assert_eq!(faulted["min_score_bps"], 0);
    assert_eq!(unknown["registered"], false);
    assert_eq!(unknown["status"], serde_json::Value::Null);
    assert_eq!(unknown["seen_ms_ago"], serde_json::Value::Null);
    assert_eq!(unknown["matchable"], false);
}

#[tokio::test]
async fn an_operator_asking_above_the_offer_is_not_matched_and_the_buyer_is_refunded() {
    let (state, payout) = new_coordinator_state(Duration::from_secs(2));
    let state_handle = state.clone();
    let base_url = spawn_coordinator(state).await;

    // The only operator online asks 5x what this buyer offers.
    let operator_identity = LocalIdentity::generate("operator@e2e");
    let register_req = RegisterRequest::sign(
        cpu_profile(&operator_identity, 5_000),
        payout_addr(1),
        &operator_identity,
    )
    .unwrap();
    let coordinator_client =
        HttpCoordinatorClient::with_config(base_url.clone(), Duration::from_secs(2), 1);
    coordinator_client.register(register_req).await.unwrap();

    let buyer_identity = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let envelope = signed_envelope(
        &buyer_identity,
        job_id,
        1_000,
        30_000,
        epoch_ms(),
        "e2e-under-ask",
    );

    let http = reqwest::Client::new();
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Refunded,
        "an offer below every ask must refund, never force a pay cut"
    );
    assert!(payout.records().is_empty());
}

#[tokio::test]
async fn escrow_holds_and_in_flight_jobs_survive_a_coordinator_restart() {
    use sha2::{Digest, Sha256};

    let dir = tempfile::tempdir().unwrap();
    let identity_path = dir.path().join("identity.json");
    let journal_path = dir.path().join("journal.jsonl");

    let durable_state = |payout: Arc<MockPayout>| {
        let identity =
            LocalIdentity::load_or_create(&identity_path, "coordinator@restart").unwrap();
        let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
        let journal_path = journal_path.clone();
        async move {
            CoordinatorState::with_journal(
                identity,
                CoordinatorConfig {
                    long_poll_timeout: Duration::from_secs(2),
                    default_funding_source: FundingSource::Organic,
                    ..CoordinatorConfig::default()
                },
                Arc::new(AuditReputationSource::new(audit.clone())),
                payout,
                audit,
                &journal_path,
                None,
            )
            .await
            .unwrap()
        }
    };

    // Life 1: operator registers, buyer's job is matched and offered,
    // the operator takes the offer — then the coordinator dies before
    // any result lands.
    let payout1 = Arc::new(MockPayout::new());
    let state1 = durable_state(payout1.clone()).await;
    let (url1, server1) = spawn_coordinator_abortable(state1.clone()).await;

    let operator_identity = LocalIdentity::generate("operator@restart");
    let operator_key = operator_identity.agent_id().pubkey_base58();
    let client1 = HttpCoordinatorClient::with_config(url1.clone(), Duration::from_secs(2), 1);
    client1
        .register(
            RegisterRequest::sign(
                cpu_profile(&operator_identity, 1_000),
                payout_addr(21),
                &operator_identity,
            )
            .unwrap(),
        )
        .await
        .unwrap();

    let buyer_identity = LocalIdentity::generate("buyer@restart");
    let job_id = Uuid::new_v4();
    let envelope = signed_envelope(
        &buyer_identity,
        job_id,
        1_000,
        60_000,
        epoch_ms(),
        "e2e-restart",
    );
    let http = reqwest::Client::new();
    let submit_resp = http
        .post(format!("{url1}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit_resp.status(), reqwest::StatusCode::ACCEPTED);

    let offer = client1
        .poll_next_job(&operator_identity.agent_id())
        .await
        .unwrap()
        .expect("the offer must reach the operator before the crash");
    assert_eq!(offer.envelope.payload.job_id, job_id);

    server1.abort();
    let _ = server1.await;

    // Life 2: rebuilt from the journal alone. The hold is still Held,
    // the job is still known and assigned to the same operator.
    let payout2 = Arc::new(MockPayout::new());
    let state2 = durable_state(payout2.clone()).await;
    assert_eq!(
        state2.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Held,
        "a restart must not lose an escrow hold"
    );
    let restored = state2.jobs().get(job_id).expect("job record restored");
    assert_eq!(
        restored.phase,
        covenant_compute_coordinator::JobPhase::Offered
    );
    assert_eq!(restored.operator_pubkey_b58, operator_key);

    let (url2, _server2) = spawn_coordinator_abortable(state2.clone()).await;

    // The operator finished the work it accepted in life 1 and submits
    // the result to the restarted coordinator.
    let output = vec![Content::text("finished across the restart")];
    let receipt = SignedWorkReceipt::sign(
        WorkReceiptPayload {
            job_id,
            operator: operator_identity.agent_id(),
            job_hash_hex: {
                let digest = Sha256::digest(offer.envelope.payload_json.as_bytes());
                digest.iter().map(|b| format!("{b:02x}")).collect()
            },
            result_hash_hex: covenant_compute_protocol::output_hash_hex(&output),
            meter: JobMeter {
                wall_ms: 5,
                tokens_in: None,
                tokens_out: None,
                gpu_seconds: None,
                finish_reason: None,
            },
            price_micro_usdc: 1_000,
            status: A2ATaskStatus::Ok,
            executed_at_ms: epoch_ms(),
            node_audit_root_hex: "cc".repeat(32),
        },
        &operator_identity,
    )
    .unwrap();
    let result_resp = http
        .post(format!("{url2}/federation/jobs/{job_id}/result"))
        .json(&covenant_compute_protocol::JobResultMessage {
            receipt,
            output: output.clone(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(result_resp.status(), reqwest::StatusCode::OK);

    assert_eq!(
        state2.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released
    );
    assert_eq!(payout2.records().len(), 1);
    assert_eq!(payout2.records()[0].amount_micro_usdc, 1_000);
    assert_eq!(payout2.records()[0].payout_address, payout_addr(21));
    assert!(payout1.records().is_empty());

    // Life 3: the settled state survives yet another restart, and the
    // double-settlement guard holds against a replayed receipt.
    let state3 = durable_state(Arc::new(MockPayout::new())).await;
    assert_eq!(
        state3.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released
    );
    assert_eq!(
        state3.jobs().get(job_id).unwrap().phase,
        covenant_compute_coordinator::JobPhase::Completed
    );
    assert_eq!(
        state3.jobs().get(job_id).unwrap().output,
        Some(output),
        "the hash-verified output survives restarts with the record"
    );
}

#[tokio::test]
async fn a_release_whose_record_write_died_heals_at_boot_and_pays_when_the_receipt_returns() {
    use sha2::{Digest, Sha256};

    let dir = tempfile::tempdir().unwrap();
    let identity_path = dir.path().join("identity.json");
    let journal_path = dir.path().join("journal.jsonl");

    let durable_state = |payout: Arc<MockPayout>| {
        let identity = LocalIdentity::load_or_create(&identity_path, "coordinator@refill").unwrap();
        let audit = Arc::new(InMemoryAuditLog::new());
        let journal_path = journal_path.clone();
        async move {
            let audit_dyn: Arc<dyn AuditLog> = audit.clone();
            let state = CoordinatorState::with_journal(
                identity,
                CoordinatorConfig {
                    long_poll_timeout: Duration::from_secs(2),
                    default_funding_source: FundingSource::Organic,
                    ..CoordinatorConfig::default()
                },
                Arc::new(AuditReputationSource::new(audit_dyn.clone())),
                payout,
                audit_dyn,
                &journal_path,
                None,
            )
            .await
            .unwrap();
            (state, audit)
        }
    };

    // Life 1: a matched job's result handler dies between the escrow
    // release (journaled) and the record write (never happens) — the
    // exact crash window submit_result cannot make atomic.
    let payout1 = Arc::new(MockPayout::new());
    let (state1, _audit1) = durable_state(payout1.clone()).await;
    let (url1, server1) = spawn_coordinator_abortable(state1.clone()).await;

    let operator_identity = LocalIdentity::generate("operator@refill");
    let client1 = HttpCoordinatorClient::with_config(url1.clone(), Duration::from_secs(2), 1);
    client1
        .register(
            RegisterRequest::sign(
                cpu_profile(&operator_identity, 1_000),
                payout_addr(22),
                &operator_identity,
            )
            .unwrap(),
        )
        .await
        .unwrap();

    let buyer_identity = LocalIdentity::generate("buyer@refill");
    let job_id = Uuid::new_v4();
    let envelope = signed_envelope(
        &buyer_identity,
        job_id,
        1_000,
        60_000,
        epoch_ms(),
        "e2e-refill",
    );
    let http = reqwest::Client::new();
    let submit_resp = http
        .post(format!("{url1}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit_resp.status(), reqwest::StatusCode::ACCEPTED);
    let offer = client1
        .poll_next_job(&operator_identity.agent_id())
        .await
        .unwrap()
        .expect("the job must be offered");

    let output = vec![Content::text("work that outlived the crash")];
    let receipt = SignedWorkReceipt::sign(
        WorkReceiptPayload {
            job_id,
            operator: operator_identity.agent_id(),
            job_hash_hex: {
                let digest = Sha256::digest(offer.envelope.payload_json.as_bytes());
                digest.iter().map(|b| format!("{b:02x}")).collect()
            },
            result_hash_hex: covenant_compute_protocol::output_hash_hex(&output),
            meter: JobMeter {
                wall_ms: 5,
                tokens_in: None,
                tokens_out: None,
                gpu_seconds: None,
                finish_reason: None,
            },
            price_micro_usdc: 1_000,
            status: A2ATaskStatus::Ok,
            executed_at_ms: epoch_ms(),
            node_audit_root_hex: "cc".repeat(32),
        },
        &operator_identity,
    )
    .unwrap();
    // The handler's first half: the fund flip lands in the journal.
    // Then the process dies — no record write, no payout push.
    state1.escrow().release(job_id, &receipt).await.unwrap();
    assert_eq!(
        state1.jobs().get(job_id).unwrap().phase,
        covenant_compute_coordinator::JobPhase::Offered,
        "the divergence under test: funds settled, record still in-flight"
    );
    server1.abort();
    let _ = server1.await;
    assert!(payout1.records().is_empty(), "no payout push happened");

    // Life 2: boot reconciliation concludes the record from the fund
    // verdict — completed, but the receipt died with the crash, so the
    // payout retry sweep can see the job and still cannot retry it.
    let payout2 = Arc::new(MockPayout::new());
    let (state2, audit2) = durable_state(payout2.clone()).await;
    let healed = state2.jobs().get(job_id).unwrap();
    assert_eq!(
        healed.phase,
        covenant_compute_coordinator::JobPhase::Completed
    );
    assert!(healed.receipt.is_none());
    let released_rows = |audit: Arc<InMemoryAuditLog>| async move {
        audit
            .recent(50)
            .await
            .unwrap()
            .iter()
            .filter(|e| {
                matches!(&e.kind, AuditKind::ComputeJobReleased { job_id: id, .. } if *id == job_id)
            })
            .count()
    };
    assert_eq!(
        released_rows(audit2.clone()).await,
        1,
        "the boot conclusion speaks for the release"
    );
    assert!(sweep_unpaid(&state2).await.is_empty());
    assert!(payout2.records().is_empty());

    // The operator re-submits the receipt: the record fills in, the
    // payout pushes, and no second release row appears.
    let (url2, _server2) = spawn_coordinator_abortable(state2.clone()).await;
    let result_resp = http
        .post(format!("{url2}/federation/jobs/{job_id}/result"))
        .json(&covenant_compute_protocol::JobResultMessage {
            receipt: receipt.clone(),
            output: output.clone(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(result_resp.status(), reqwest::StatusCode::OK);
    let ack: covenant_compute_protocol::JobResultAck = result_resp.json().await.unwrap();
    assert_eq!(
        ack.settled,
        covenant_compute_protocol::ResultSettlement::Released,
        "the fill-in acks the release the crash swallowed"
    );
    let filled = state2.jobs().get(job_id).unwrap();
    assert_eq!(
        filled.phase,
        covenant_compute_coordinator::JobPhase::Completed
    );
    assert!(filled.receipt.is_some());
    assert_eq!(filled.output, Some(output.clone()));
    assert_eq!(payout2.records().len(), 1);
    assert_eq!(payout2.records()[0].amount_micro_usdc, 1_000);
    assert_eq!(payout2.records()[0].payout_address, payout_addr(22));
    assert_eq!(
        released_rows(audit2.clone()).await,
        1,
        "the fill-in must not double-audit the release"
    );

    // A receipt the record already carries is a plain replay: the
    // coordinator idempotently echoes the concluded verdict so a node
    // redelivering after a lost ack can finish crediting, and never
    // re-pays or re-audits.
    let replay_resp = http
        .post(format!("{url2}/federation/jobs/{job_id}/result"))
        .json(&covenant_compute_protocol::JobResultMessage { receipt, output })
        .send()
        .await
        .unwrap();
    assert_eq!(replay_resp.status(), reqwest::StatusCode::OK);
    let replay_ack: covenant_compute_protocol::JobResultAck = replay_resp.json().await.unwrap();
    assert_eq!(
        replay_ack.settled,
        covenant_compute_protocol::ResultSettlement::Released,
        "a replay echoes the released verdict the first submission returned"
    );
    assert_eq!(payout2.records().len(), 1, "a replay must never re-pay");
    assert_eq!(
        released_rows(audit2.clone()).await,
        1,
        "a replay must never re-audit the release"
    );
}

#[tokio::test]
async fn a_coordinator_restart_mid_job_does_not_cost_the_operator_its_pay() {
    use covenant_compute_node::{ExecutionOutcome, ExecutorError, JobExecutor, ResultOutbox};
    use covenant_compute_protocol::JobEnvelopePayload;
    use tokio::sync::Notify;

    // Finishes only when the test says so — the lever that puts the
    // coordinator's death exactly between the node's accept and its
    // result push, the window a real deploy hits at random.
    struct GatedExecutor {
        started: Arc<Notify>,
        release: Arc<Notify>,
    }

    #[async_trait::async_trait]
    impl JobExecutor for GatedExecutor {
        async fn execute(
            &self,
            _job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<ExecutionOutcome, ExecutorError> {
            self.started.notify_one();
            self.release.notified().await;
            Ok(ExecutionOutcome {
                output: vec![Content::text("finished during the outage")],
                wall_ms: 1,
                tokens_in: None,
                tokens_out: None,
                finish_reason: None,
            })
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let identity_path = dir.path().join("identity.json");
    let journal_path = dir.path().join("journal.jsonl");
    let durable_state = |payout: Arc<MockPayout>| {
        let identity = LocalIdentity::load_or_create(&identity_path, "coordinator@deploy").unwrap();
        let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
        let journal_path = journal_path.clone();
        async move {
            CoordinatorState::with_journal(
                identity,
                CoordinatorConfig {
                    long_poll_timeout: Duration::from_secs(2),
                    default_funding_source: FundingSource::Organic,
                    ..CoordinatorConfig::default()
                },
                Arc::new(AuditReputationSource::new(audit.clone())),
                payout,
                audit,
                &journal_path,
                None,
            )
            .await
            .unwrap()
        }
    };

    // Life 1. The node talks to the stable address owned by the
    // killable proxy (see `spawn_proxy`).
    let payout1 = Arc::new(MockPayout::new());
    let state1 = durable_state(payout1.clone()).await;
    let coordinator_pubkey_b58 = state1.coordinator_pubkey_b58();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream1 = listener.local_addr().unwrap();
    let server1 = tokio::spawn(async move {
        let _ = axum::serve(listener, router(state1)).await;
    });
    let (proxy_addr, proxy1) = spawn_proxy(None, upstream1).await;
    let base_url = format!("http://{proxy_addr}");

    let operator_identity = LocalIdentity::generate("operator@deploy");
    let profile = cpu_profile(&operator_identity, 1_000);
    let client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(2),
        1,
    ));
    client
        .register(
            RegisterRequest::sign(profile.clone(), payout_addr(16), &operator_identity).unwrap(),
        )
        .await
        .unwrap();

    let buyer_identity = LocalIdentity::generate("buyer@deploy");
    let job_id = Uuid::new_v4();
    let envelope = signed_envelope(
        &buyer_identity,
        job_id,
        1_000,
        60_000,
        epoch_ms(),
        "e2e-deploy",
    );
    let http = reqwest::Client::new();
    let submit_resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit_resp.status(), reqwest::StatusCode::ACCEPTED);

    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let outbox = Arc::new(ResultOutbox::in_memory());
    let earnings = Arc::new(InMemoryEarningsLedger::new());
    let node = Arc::new(
        Node::new(
            operator_identity,
            profile,
            client,
            Arc::new(GatedExecutor {
                started: started.clone(),
                release: release.clone(),
            }),
            earnings.clone(),
            Arc::new(InMemoryAuditLog::new()),
            NodeConfig {
                coordinator_pubkey_b58,
                max_in_flight: 4,
                preempt_grace: Duration::from_secs(2),
                fee_bps: 0,
            },
        )
        .with_outbox(outbox.clone()),
    );

    // The node takes the job; mid-execution the coordinator dies.
    let run = {
        let node = node.clone();
        tokio::spawn(async move { node.run_once().await })
    };
    started.notified().await;
    proxy1.abort();
    let _ = proxy1.await;
    server1.abort();
    let _ = server1.await;
    release.notify_one();

    // The work concluded against a dead coordinator: the job does not
    // fail, nothing is credited, and the result waits in the outbox.
    let outcome = run
        .await
        .unwrap()
        .expect("an undeliverable result must not fail the job")
        .expect("the offer was delivered");
    assert_eq!(outcome.receipt.receipt.status, A2ATaskStatus::Ok);
    assert_eq!(outbox.pending().len(), 1);
    assert_eq!(earnings.unpaid_total_micro_usdc().await, 0);
    assert!(payout1.records().is_empty());

    // Life 2: the deploy finishes — the same stable address, state
    // rebuilt from the journal (hold still Held, job still assigned).
    let payout2 = Arc::new(MockPayout::new());
    let state2 = durable_state(payout2.clone()).await;
    assert_eq!(
        state2.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Held
    );
    let listener2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream2 = listener2.local_addr().unwrap();
    let state2_handle = state2.clone();
    let _server2 = tokio::spawn(async move {
        let _ = axum::serve(listener2, router(state2)).await;
    });
    let (_, _proxy2) = spawn_proxy(Some(proxy_addr), upstream2).await;

    // One drain tick: the queued result lands, the escrow releases,
    // the payout pushes, and the operator's books finally credit.
    assert_eq!(node.drain_outbox().await, 1);
    assert!(outbox.pending().is_empty());
    assert_eq!(
        state2_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released
    );
    let record = state2_handle.jobs().get(job_id).unwrap();
    assert_eq!(
        record.phase,
        covenant_compute_coordinator::JobPhase::Completed
    );
    assert!(record.receipt.is_some());
    assert_eq!(payout2.records().len(), 1);
    assert_eq!(payout2.records()[0].amount_micro_usdc, 1_000);
    assert_eq!(payout2.records()[0].payout_address, payout_addr(16));
    assert_eq!(earnings.unpaid_total_micro_usdc().await, 1_000);
}

/// The same deploy window, hit while the buyer is DRAINING A LIVE
/// FEED: the in-memory stream buffer dies with the coordinator (the
/// documented degradation), but the dispatch it previews must not —
/// the buyer's poll loops ride the outage, the node's result lands on
/// the restarted coordinator, and the buyer walks away with the whole
/// verified output and the operator with its pay. Only the feed's
/// tail is lost, and `stream_matched_output` says so.
#[tokio::test]
async fn a_coordinator_restart_mid_stream_still_delivers_the_verified_result() {
    use covenant_compute_buyer::{stream_and_verify, submit_streaming, BuyerConfig, JobRequest};
    use covenant_compute_node::{
        ChunkSink, ExecutionOutcome, ExecutorError, JobExecutor, ResultOutbox,
    };
    use tokio::sync::Notify;

    // Streams "hello " and then holds the job open until the test says
    // the deploy is over — pinning the coordinator's death to the
    // middle of the feed.
    struct GatedStreamingExecutor {
        release: Arc<Notify>,
    }

    #[async_trait::async_trait]
    impl JobExecutor for GatedStreamingExecutor {
        async fn execute(
            &self,
            _job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<ExecutionOutcome, ExecutorError> {
            unreachable!("the envelope asks for streaming")
        }

        async fn execute_streaming(
            &self,
            _job: &JobEnvelopePayload,
            _deadline: Duration,
            sink: ChunkSink,
        ) -> Result<ExecutionOutcome, ExecutorError> {
            let _ = sink.send("hello ".into()).await;
            self.release.notified().await;
            let _ = sink.send("world".into()).await;
            Ok(ExecutionOutcome {
                output: vec![Content::text("hello world")],
                wall_ms: 1,
                tokens_in: None,
                tokens_out: None,
                finish_reason: None,
            })
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let identity_path = dir.path().join("identity.json");
    let journal_path = dir.path().join("journal.jsonl");
    let durable_state = |payout: Arc<MockPayout>| {
        let identity =
            LocalIdentity::load_or_create(&identity_path, "coordinator@stream-deploy").unwrap();
        let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
        let journal_path = journal_path.clone();
        async move {
            CoordinatorState::with_journal(
                identity,
                CoordinatorConfig {
                    long_poll_timeout: Duration::from_secs(2),
                    default_funding_source: FundingSource::Organic,
                    ..CoordinatorConfig::default()
                },
                Arc::new(AuditReputationSource::new(audit.clone())),
                payout,
                audit,
                &journal_path,
                None,
            )
            .await
            .unwrap()
        }
    };

    // Life 1.
    let payout1 = Arc::new(MockPayout::new());
    let state1 = durable_state(payout1.clone()).await;
    let coordinator_pubkey_b58 = state1.coordinator_pubkey_b58();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream1 = listener.local_addr().unwrap();
    let server1 = tokio::spawn(async move {
        let _ = axum::serve(listener, router(state1)).await;
    });
    let (proxy_addr, proxy1) = spawn_proxy(None, upstream1).await;
    let base_url = format!("http://{proxy_addr}");

    let operator_identity = LocalIdentity::generate("operator@stream-deploy");
    let profile = cpu_profile(&operator_identity, 1_000);
    let client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(2),
        1,
    ));
    client
        .register(
            RegisterRequest::sign(profile.clone(), payout_addr(17), &operator_identity).unwrap(),
        )
        .await
        .unwrap();

    // The buyer submits a streaming job against the stable address and
    // drains the feed exactly like a real client: same submit + poll
    // code every covenantd/MCP surface runs.
    let buyer_identity = LocalIdentity::generate("buyer@stream-deploy");
    let buyer_config = BuyerConfig {
        coordinator_url: base_url.clone(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    let http = reqwest::Client::new();
    let job_id = Uuid::new_v4();
    let envelope = submit_streaming(
        &http,
        &buyer_config,
        &buyer_identity,
        job_id,
        JobRequest {
            min_reputation_bps: None,
            kind: JobKind::BatchJob,
            model: None,
            gpu_class: None,
            min_vram_gb: None,
            input: vec![Content::text("stream: hello world")],
            price_micro_usdc: 1_000,
            deadline_ms: 60_000,
        },
    )
    .await
    .unwrap();

    let fed = Arc::new(std::sync::Mutex::new(String::new()));
    let first_chunk = Arc::new(Notify::new());
    let buyer_task = {
        let fed = fed.clone();
        let first_chunk = first_chunk.clone();
        let http = http.clone();
        let config = buyer_config.clone();
        tokio::spawn(async move {
            stream_and_verify(&http, &config, &buyer_identity, envelope, |chunk| {
                fed.lock().unwrap().push_str(chunk);
                first_chunk.notify_one();
            })
            .await
        })
    };

    let release = Arc::new(Notify::new());
    let outbox = Arc::new(ResultOutbox::in_memory());
    let earnings = Arc::new(InMemoryEarningsLedger::new());
    let node = Arc::new(
        Node::new(
            operator_identity,
            profile,
            client,
            Arc::new(GatedStreamingExecutor {
                release: release.clone(),
            }),
            earnings.clone(),
            Arc::new(InMemoryAuditLog::new()),
            NodeConfig {
                coordinator_pubkey_b58,
                max_in_flight: 4,
                preempt_grace: Duration::from_secs(2),
                fee_bps: 0,
            },
        )
        .with_outbox(outbox.clone()),
    );
    let run = {
        let node = node.clone();
        tokio::spawn(async move { node.run_once().await })
    };

    // The feed is provably live end to end — the buyer has drained the
    // first chunk — and THEN the deploy hits. Holding the address dark
    // past one poll interval guarantees the buyer polls into the
    // outage, the exact failure that used to abort the dispatch.
    first_chunk.notified().await;
    assert_eq!(fed.lock().unwrap().as_str(), "hello ");
    proxy1.abort();
    let _ = proxy1.await;
    server1.abort();
    let _ = server1.await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Life 2: same stable address, state rebuilt from the journal —
    // the job is still Accepted, the hold still Held, but the stream
    // buffer is gone (deliberately not journaled).
    let payout2 = Arc::new(MockPayout::new());
    let state2 = durable_state(payout2.clone()).await;
    assert_eq!(
        state2.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Held
    );
    let listener2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream2 = listener2.local_addr().unwrap();
    let state2_handle = state2.clone();
    let _server2 = tokio::spawn(async move {
        let _ = axum::serve(listener2, router(state2)).await;
    });
    let (_, _proxy2) = spawn_proxy(Some(proxy_addr), upstream2).await;

    // The deploy is over; the job finishes against the restarted
    // coordinator. The node's post-restart chunk push is refused (the
    // rebuilt buffer never saw the first batch), which disables the
    // relay — the receipt path, not the feed, carries the result.
    release.notify_one();
    let outcome = run
        .await
        .unwrap()
        .expect("the job must conclude, not fail")
        .expect("the offer was delivered");
    assert_eq!(outcome.receipt.receipt.status, A2ATaskStatus::Ok);

    // The buyer's dispatch rode the outage: whole verified output,
    // clipped preview, and the mismatch flag says exactly that.
    let streamed = buyer_task
        .await
        .unwrap()
        .expect("a deploy mid-stream must not fail the dispatch");
    assert_eq!(
        streamed.outcome.receipt.receipt.status,
        A2ATaskStatus::Ok,
        "the receipt re-verified end to end"
    );
    let final_text: String = streamed
        .outcome
        .output
        .iter()
        .filter_map(|c| match c {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(final_text, "hello world");
    assert_eq!(
        fed.lock().unwrap().as_str(),
        "hello ",
        "the feed's tail died with the coordinator, and only the tail"
    );
    assert!(
        !streamed.stream_matched_output,
        "the outcome grades its own clipped preview honestly"
    );

    // Money: exactly one release, on the restarted coordinator's books.
    assert_eq!(
        state2_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released
    );
    let record = state2_handle.jobs().get(job_id).unwrap();
    assert_eq!(
        record.phase,
        covenant_compute_coordinator::JobPhase::Completed
    );
    assert!(record.receipt.is_some());
    assert!(payout1.records().is_empty());
    assert_eq!(payout2.records().len(), 1);
    assert_eq!(payout2.records()[0].amount_micro_usdc, 1_000);
    assert_eq!(payout2.records()[0].payout_address, payout_addr(17));
    assert_eq!(earnings.unpaid_total_micro_usdc().await, 1_000);
}

/// The supply-side crash the deadline sweep exists for: a node ACCEPTS
/// a job over the real wire and then dies mid-execution — no result,
/// no refusal, just silence. At the deadline the sweep refunds the
/// buyer, attributes the fault to the operator that went dark, and the
/// buyer's own dispatch call — already polling — is told "refunded".
/// A vanished node can cost the buyer only time, never money.
#[tokio::test]
async fn a_node_that_dies_mid_execution_costs_the_buyer_nothing_and_faults_the_operator() {
    use covenant_compute_buyer::{dispatch_and_verify, BuyerConfig, BuyerError, JobRequest};
    use covenant_compute_node::{ExecutionOutcome, ExecutorError, JobExecutor};
    use tokio::sync::Notify;

    // Hangs forever once started — the executor of a machine about to
    // lose power. `started` pins the node's death to mid-execution.
    struct HangingExecutor {
        started: Arc<Notify>,
    }

    #[async_trait::async_trait]
    impl JobExecutor for HangingExecutor {
        async fn execute(
            &self,
            _job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<ExecutionOutcome, ExecutorError> {
            self.started.notify_one();
            std::future::pending::<()>().await;
            unreachable!()
        }
    }

    let (state, payout) = new_coordinator_state(Duration::from_secs(2));
    let state_handle = state.clone();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@vanishes");
    let operator_pubkey = operator_identity.agent_id().pubkey_base58();
    let coordinator_pubkey_b58 = state_handle.coordinator_pubkey_b58();
    let profile = cpu_profile(&operator_identity, 1_000);
    let client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(2),
        1,
    ));
    client
        .register(
            RegisterRequest::sign(profile.clone(), payout_addr(10), &operator_identity).unwrap(),
        )
        .await
        .unwrap();

    // The buyer dispatches with the real client and keeps polling — it
    // must be told "refunded", not left hanging or charged.
    let buyer_identity = LocalIdentity::generate("buyer@vanishes");
    let buyer_config = BuyerConfig {
        coordinator_url: base_url.clone(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    let http = reqwest::Client::new();
    let deadline_ms = 2_000;
    let buyer_task = {
        let http = http.clone();
        let config = buyer_config.clone();
        tokio::spawn(async move {
            dispatch_and_verify(
                &http,
                &config,
                &buyer_identity,
                JobRequest {
                    min_reputation_bps: None,
                    kind: JobKind::BatchJob,
                    model: None,
                    gpu_class: None,
                    min_vram_gb: None,
                    input: vec![Content::text("about to be abandoned")],
                    price_micro_usdc: 1_000,
                    deadline_ms,
                },
            )
            .await
        })
    };

    let started = Arc::new(Notify::new());
    let earnings = Arc::new(InMemoryEarningsLedger::new());
    let node = Arc::new(Node::new(
        operator_identity,
        profile,
        client,
        Arc::new(HangingExecutor {
            started: started.clone(),
        }),
        earnings.clone(),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    ));
    let run = {
        let node = node.clone();
        tokio::spawn(async move { node.run_once().await })
    };

    // Accepted and mid-execution — then the machine dies.
    started.notified().await;
    run.abort();
    let _ = run.await;

    // The deadline passes on the real clock; the sweep ticks like the
    // deployed coordinator's periodic task until it catches the corpse.
    let mut refunded = Vec::new();
    for _ in 0..60 {
        refunded = sweep_expired(&state_handle, epoch_ms()).await;
        if !refunded.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(refunded.len(), 1, "the abandoned job was swept");
    let job_id = refunded[0];

    // The polling buyer hears the refund, owes nothing, gets nothing.
    let err = buyer_task
        .await
        .unwrap()
        .expect_err("a vanished node's job must resolve, not hang the buyer");
    assert!(
        matches!(&err, BuyerError::NotServed { job_id: id, status, reason, .. } if *id == job_id && status == "refunded" && reason.as_deref() == Some("deadline_expired")),
        "got: {err}"
    );

    // Money: hold refunded, nothing paid, nothing credited.
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Refunded
    );
    assert_eq!(
        state_handle.jobs().get(job_id).unwrap().phase,
        covenant_compute_coordinator::JobPhase::Refunded
    );
    assert!(payout.records().is_empty());
    assert_eq!(earnings.unpaid_total_micro_usdc().await, 0);

    // Attribution: the refund row names the operator that went dark,
    // and reputation counts the fault.
    let events = state_handle.audit().recent(10).await.unwrap();
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        AuditKind::ComputeJobRefunded { job_id: id, reason, operator_pubkey_b58: Some(op) }
            if *id == job_id && reason == "deadline_expired" && *op == operator_pubkey
    )));
    let reputation: serde_json::Value = http
        .get(format!(
            "{base_url}/federation/operators/{operator_pubkey}/reputation"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reputation["released"], 0);
    assert_eq!(reputation["faults"], 1);
}

/// The supply-side deploy window: the node process dies mid-execution
/// but comes back within the deadline. The accepted-jobs book re-serves
/// the job at boot, so the buyer's already-polling dispatch resolves to
/// the verified result — not a refund — and the operator keeps the pay
/// and a clean fault record. The refund sweep, given every chance,
/// finds nothing to claim.
#[tokio::test]
async fn a_node_restart_mid_job_re_serves_it_and_the_operator_still_earns() {
    use covenant_compute_buyer::{dispatch_and_verify, BuyerConfig, JobRequest};
    use covenant_compute_node::{
        AcceptedBook, EchoExecutor, ExecutionOutcome, ExecutorError, JobExecutor,
    };
    use tokio::sync::Notify;

    // Hangs forever once started — the executor of a process about to
    // die. `started` pins the node's death to mid-execution.
    struct HangingExecutor {
        started: Arc<Notify>,
    }

    #[async_trait::async_trait]
    impl JobExecutor for HangingExecutor {
        async fn execute(
            &self,
            _job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<ExecutionOutcome, ExecutorError> {
            self.started.notify_one();
            std::future::pending::<()>().await;
            unreachable!()
        }
    }

    let (state, payout) = new_coordinator_state(Duration::from_secs(2));
    let state_handle = state.clone();
    let base_url = spawn_coordinator(state).await;

    // The operator's persistent home: the identity earnings accrue to
    // and the book that remembers the job across the death.
    let dir = tempfile::tempdir().unwrap();
    let identity_path = dir.path().join("identity.json");
    let book_path = dir.path().join("accepted.jsonl");

    let operator1 = LocalIdentity::load_or_create(&identity_path, "operator@reborn").unwrap();
    let operator_pubkey = operator1.agent_id().pubkey_base58();
    let coordinator_pubkey_b58 = state_handle.coordinator_pubkey_b58();
    let profile = cpu_profile(&operator1, 1_000);
    let client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(2),
        1,
    ));
    client
        .register(RegisterRequest::sign(profile.clone(), payout_addr(11), &operator1).unwrap())
        .await
        .unwrap();

    // The buyer dispatches with the real client and keeps polling
    // through the whole node outage.
    let buyer_identity = LocalIdentity::generate("buyer@reborn");
    let buyer_config = BuyerConfig {
        coordinator_url: base_url.clone(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    let http = reqwest::Client::new();
    let buyer_task = {
        let http = http.clone();
        let config = buyer_config.clone();
        tokio::spawn(async move {
            dispatch_and_verify(
                &http,
                &config,
                &buyer_identity,
                JobRequest {
                    min_reputation_bps: None,
                    kind: JobKind::BatchJob,
                    model: None,
                    gpu_class: None,
                    min_vram_gb: None,
                    input: vec![Content::text("survive the restart")],
                    price_micro_usdc: 1_000,
                    deadline_ms: 15_000,
                },
            )
            .await
        })
    };

    // Life 1: accept lands, the book remembers, the process dies
    // holding the job.
    let started = Arc::new(Notify::new());
    let node1 = Arc::new(
        Node::new(
            operator1,
            profile.clone(),
            client.clone(),
            Arc::new(HangingExecutor {
                started: started.clone(),
            }),
            Arc::new(InMemoryEarningsLedger::new()),
            Arc::new(InMemoryAuditLog::new()),
            NodeConfig {
                coordinator_pubkey_b58: coordinator_pubkey_b58.clone(),
                max_in_flight: 4,
                preempt_grace: Duration::from_secs(2),
                fee_bps: 0,
            },
        )
        .with_accepted_book(Arc::new(AcceptedBook::open(&book_path).unwrap())),
    );
    let run = {
        let node = node1.clone();
        tokio::spawn(async move { node.run_once().await })
    };
    started.notified().await;
    run.abort();
    let _ = run.await;

    let booked = AcceptedBook::open(&book_path).unwrap().pending();
    assert_eq!(booked.len(), 1, "the accepted job survived the death");
    let job_id = booked[0].job_id;
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Held,
        "the coordinator still holds the buyer's funds for the assigned node"
    );

    // Life 2: the same operator restarts on its home; boot recovery
    // re-runs the job through the normal path.
    let operator2 = LocalIdentity::load_or_create(&identity_path, "operator@reborn").unwrap();
    let earnings2 = Arc::new(InMemoryEarningsLedger::new());
    let book2 = Arc::new(AcceptedBook::open(&book_path).unwrap());
    let node2 = Node::new(
        operator2,
        profile,
        client,
        Arc::new(EchoExecutor),
        earnings2.clone(),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    )
    .with_accepted_book(book2.clone());
    assert_eq!(node2.recover_accepted().await, 1);
    assert!(book2.pending().is_empty(), "the re-served job settled");

    // The buyer's dispatch resolves to the verified result — the
    // outage cost it nothing but latency.
    let outcome = buyer_task
        .await
        .unwrap()
        .expect("a re-served job must resolve the dispatch, not refund it");
    assert_eq!(outcome.receipt.receipt.job_id, job_id);
    assert_eq!(outcome.receipt.receipt.status, A2ATaskStatus::Ok);
    assert!(!outcome.output.is_empty());

    // Money: released to the operator, paid to its payout address,
    // credited in its books.
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released
    );
    let record = state_handle.jobs().get(job_id).unwrap();
    assert_eq!(
        record.phase,
        covenant_compute_coordinator::JobPhase::Completed
    );
    assert!(record.receipt.is_some());
    assert_eq!(payout.records().len(), 1);
    assert_eq!(payout.records()[0].amount_micro_usdc, 1_000);
    assert_eq!(payout.records()[0].payout_address, payout_addr(11));
    assert_eq!(earnings2.unpaid_total_micro_usdc().await, 1_000);

    // The sweep, run well past the deadline, has nothing to refund —
    // and the operator's record shows a serve, not a fault.
    assert!(sweep_expired(&state_handle, epoch_ms() + 60_000)
        .await
        .is_empty());
    let reputation: serde_json::Value = http
        .get(format!(
            "{base_url}/federation/operators/{operator_pubkey}/reputation"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reputation["released"], 1);
    assert_eq!(reputation["faults"], 0);
}

#[tokio::test]
async fn a_prefunded_buyer_deposits_once_spends_across_jobs_and_survives_restart() {
    use covenant_compute_coordinator::{MockRail, VerifiedDeposit};

    let dir = tempfile::tempdir().unwrap();
    let identity_path = dir.path().join("identity.json");
    let journal_path = dir.path().join("journal.jsonl");
    let rail = Arc::new(MockRail::new());

    let prefunded_state = |rail: Arc<MockRail>, payout: Arc<MockPayout>| {
        let identity =
            LocalIdentity::load_or_create(&identity_path, "coordinator@prefunded").unwrap();
        let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
        let journal_path = journal_path.clone();
        async move {
            CoordinatorState::with_journal(
                identity,
                CoordinatorConfig {
                    long_poll_timeout: Duration::from_secs(2),
                    default_funding_source: FundingSource::Organic,
                    require_prefunded_buyers: true,
                    ..CoordinatorConfig::default()
                },
                Arc::new(AuditReputationSource::new(audit.clone())),
                payout,
                audit,
                &journal_path,
                Some(rail),
            )
            .await
            .unwrap()
        }
    };

    let payout = Arc::new(MockPayout::new());
    let state = prefunded_state(rail.clone(), payout.clone()).await;
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state.clone()).await;

    let buyer_identity = LocalIdentity::generate("buyer@prefunded");
    let buyer_key = buyer_identity.agent_id().pubkey_base58();
    let http = reqwest::Client::new();

    // Unfunded: the job is refused with 402 before any matching runs.
    let refused = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&signed_envelope(
            &buyer_identity,
            Uuid::new_v4(),
            1_000,
            30_000,
            epoch_ms(),
            "prefunded-unfunded",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::PAYMENT_REQUIRED);

    // A claim the rail has never seen verifies nothing.
    let unknown = http
        .post(format!("{base_url}/federation/buyers/deposit"))
        .json(&serde_json::json!({
            "buyer_pubkey_b58": buyer_key,
            "deposit_id": "sig-nowhere",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), reqwest::StatusCode::NOT_FOUND);

    // The payment confirms on the rail; claiming it for a different
    // buyer than the rail attributes it to is refused.
    rail.preload(VerifiedDeposit {
        deposit_id: "sig-deposit-1".into(),
        buyer_pubkey_b58: buyer_key.clone(),
        amount_micro_usdc: 10_000,
    });
    let thief = http
        .post(format!("{base_url}/federation/buyers/deposit"))
        .json(&serde_json::json!({
            "buyer_pubkey_b58": "someone-else",
            "deposit_id": "sig-deposit-1",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(thief.status(), reqwest::StatusCode::BAD_REQUEST);

    // The rightful claim credits (through the buyer crate's own client
    // — the same code path the MCP deposit tool runs); a retry of the
    // same claim doesn't credit twice.
    let buyer_config = covenant_compute_buyer::BuyerConfig {
        coordinator_url: base_url.clone(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    let credited = covenant_compute_buyer::claim_deposit(
        &http,
        &buyer_config,
        &buyer_identity,
        "sig-deposit-1",
    )
    .await
    .unwrap();
    assert!(credited.credited);
    assert_eq!(credited.amount_micro_usdc, 10_000);
    let retried = covenant_compute_buyer::claim_deposit(
        &http,
        &buyer_config,
        &buyer_identity,
        "sig-deposit-1",
    )
    .await
    .unwrap();
    assert!(!retried.credited);
    assert_eq!(retried.amount_micro_usdc, 10_000);

    // The funds view an agent plans against: balance plus this
    // deployment's top-up instructions, from the rail itself. The
    // balance half is a signed read — deposit signatures are public
    // on-chain, so neither a replayed claim (checked above: deposit
    // facts only) nor a bare pubkey may read the running balance.
    let funds =
        covenant_compute_buyer::funds_with_deposit_info(&http, &buyer_config, &buyer_identity)
            .await
            .unwrap();
    assert_eq!(funds["balance"]["available_micro_usdc"], 10_000);
    assert_eq!(funds["balance"]["prefunding_enforced"], true);
    assert_eq!(funds["deposit_info"]["configured"], true);
    let unsigned_balance = http
        .get(format!("{base_url}/federation/buyers/{buyer_key}/balance"))
        .send()
        .await
        .unwrap();
    assert_eq!(unsigned_balance.status(), 401);

    // Funded: the same buyer's job now runs the full loop against a
    // real node and pays the operator from the deposit.
    let operator_identity = LocalIdentity::generate("operator@prefunded");
    let profile = cpu_profile(&operator_identity, 1_000);
    let coordinator_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(2),
        1,
    ));
    coordinator_client
        .register(
            RegisterRequest::sign(profile.clone(), payout_addr(23), &operator_identity).unwrap(),
        )
        .await
        .unwrap();
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

    let job_id = Uuid::new_v4();
    let submitted = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&signed_envelope(
            &buyer_identity,
            job_id,
            1_000,
            30_000,
            epoch_ms(),
            "prefunded-happy",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(submitted.status(), reqwest::StatusCode::ACCEPTED);
    let outcome = node.run_once().await.unwrap().expect("offer reaches node");
    assert_eq!(outcome.job_id, job_id);
    assert_eq!(
        state.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released
    );
    assert_eq!(payout.records().len(), 1);

    // Spent money is gone from the balance; the remainder is still
    // available.
    let funds =
        covenant_compute_buyer::funds_with_deposit_info(&http, &buyer_config, &buyer_identity)
            .await
            .unwrap();
    assert_eq!(funds["balance"]["deposited_micro_usdc"], 10_000);
    assert_eq!(funds["balance"]["charged_micro_usdc"], 1_000);
    assert_eq!(funds["balance"]["available_micro_usdc"], 9_000);
    assert_eq!(funds["balance"]["prefunding_enforced"], true);

    // The balance is derived state, so it survives a restart exactly:
    // 9_001 overdraws by one micro-USDC, 9_000 clears the funds check
    // (and then refunds as no operator is registered on the new life).
    let state2 = prefunded_state(rail, Arc::new(MockPayout::new())).await;
    let base_url2 = spawn_coordinator(state2).await;
    let over = http
        .post(format!("{base_url2}/federation/jobs"))
        .json(&signed_envelope(
            &buyer_identity,
            Uuid::new_v4(),
            9_001,
            30_000,
            epoch_ms(),
            "prefunded-over-after-restart",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(over.status(), reqwest::StatusCode::PAYMENT_REQUIRED);
    let exact = http
        .post(format!("{base_url2}/federation/jobs"))
        .json(&signed_envelope(
            &buyer_identity,
            Uuid::new_v4(),
            9_000,
            30_000,
            epoch_ms(),
            "prefunded-exact-after-restart",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(
        exact.status(),
        reqwest::StatusCode::CONFLICT,
        "the funds check clears at exactly the available balance; the 409 is the no-operator refund"
    );
}

/// A coordinator booted without an inbound rail — the binary's default
/// when no rail env is set — has nothing to verify a money claim
/// against, so deposit and bond claims must refuse with a verdict that
/// names the deployment gap rather than 404-ing or crediting blind.
#[tokio::test]
async fn deposit_and_bond_claims_refuse_when_no_inbound_rail_is_configured() {
    let (state, _payout) = new_coordinator_state(Duration::from_secs(2));
    let base_url = spawn_coordinator(state).await;
    let http = reqwest::Client::new();

    let deposit = http
        .post(format!("{base_url}/federation/buyers/deposit"))
        .json(&serde_json::json!({"buyer_pubkey_b58": "any-buyer", "deposit_id": "sig-1"}))
        .send()
        .await
        .unwrap();
    assert_eq!(deposit.status(), reqwest::StatusCode::CONFLICT);
    assert!(deposit
        .text()
        .await
        .unwrap()
        .contains("no inbound rail is configured"));

    let bond = http
        .post(format!("{base_url}/federation/operators/bond"))
        .json(&serde_json::json!({"operator_pubkey_b58": "any-operator", "bond_id": "sig-2"}))
        .send()
        .await
        .unwrap();
    assert_eq!(bond.status(), reqwest::StatusCode::CONFLICT);
    assert!(bond
        .text()
        .await
        .unwrap()
        .contains("no inbound rail is configured"));
}

#[tokio::test]
async fn no_capable_operator_refunds_immediately() {
    let (state, _payout) = new_coordinator_state(Duration::from_secs(2));
    let state_handle = state.clone();
    let base_url = spawn_coordinator(state).await;
    // Deliberately no operator registered at all.

    let buyer_identity = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let now_ms = epoch_ms();
    let envelope = signed_envelope(
        &buyer_identity,
        job_id,
        1_000,
        30_000,
        now_ms,
        "e2e-no-operator",
    );

    let http = reqwest::Client::new();
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Refunded
    );
    assert_eq!(
        state_handle.jobs().get(job_id).unwrap().refund_reason,
        Some(covenant_compute_protocol::RefundReason::AdmissionFailed),
        "the terminal no-operator record carries its reason"
    );
    let events = state_handle.audit().recent(10).await.unwrap();
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        AuditKind::ComputeJobRefunded { job_id: id, reason, operator_pubkey_b58: None } if *id == job_id && reason == "admission_failed"
    )));
}

#[tokio::test]
async fn a_below_market_offer_is_refused_with_the_price_to_beat() {
    let (state, _payout) = new_coordinator_state(Duration::from_secs(2));
    let state_handle = state.clone();
    let base_url = spawn_coordinator(state).await;

    // One operator online, asking 1_000 micro-USDC per job.
    let operator_identity = LocalIdentity::generate("operator@e2e");
    let profile = cpu_profile(&operator_identity, 1_000);
    let client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        2,
    ));
    let register_req = RegisterRequest::sign(profile, payout_addr(3), &operator_identity).unwrap();
    assert!(client.register(register_req).await.unwrap().accepted);

    // A buyer offers 100: real supply serves this shape, just above the
    // offer. The refusal must diagnose price, not claim an empty market.
    let buyer_identity = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let envelope = signed_envelope(
        &buyer_identity,
        job_id,
        100,
        30_000,
        epoch_ms(),
        "e2e-under-market",
    );

    let http = reqwest::Client::new();
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("1000") && body.contains("raise"),
        "the refusal names the ask to beat and tells the buyer to raise: {body}"
    );
    assert!(
        !body.contains("no operator is currently serving"),
        "supply exists, so the empty-market line must not fire: {body}"
    );

    // The improved message changes only the diagnosis: the hold is still
    // refunded immediately, exactly as before.
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Refunded
    );
}

/// The demanded hardware ask (`--gpu-class` / `--min-vram-gb`) enforced
/// end to end, buyer signature to matcher. The only operator is a 24 GB
/// rtx-4090; a job that signs a requirement it cannot meet — more VRAM, or
/// a different GPU class — is refused at admission with the hold refunded,
/// never dispatched to a node that would fail it, while a requirement it
/// CAN meet still lands. This is the path the protocol's `satisfies()`
/// unit tests and the buyer lib's cheapest-ask filter don't cover: the
/// requirement riding the signed envelope through the real submit handler.
#[tokio::test]
async fn a_demanded_hardware_ask_the_only_operator_cannot_meet_refuses_at_admission() {
    let (state, _payout) = new_coordinator_state(Duration::from_secs(2));
    let state_handle = state.clone();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@e2e");
    let profile = CapabilityProfile {
        operator: operator_identity.agent_id(),
        hardware: HardwareClass::ConsumerGpu {
            model: "rtx-4090".into(),
        },
        vram_gb: 24,
        models_served: vec!["any".into()],
        job_kinds: vec![JobKind::BatchJob],
        price: PriceAsk {
            unit: PriceUnit::PerJob,
            micro_usdc: 1_000,
        },
        tee_capable: false,
        kind_prices: Vec::new(),
        kind_models: Vec::new(),
    };
    let client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        2,
    ));
    let register_req = RegisterRequest::sign(profile, payout_addr(4), &operator_identity).unwrap();
    assert!(client.register(register_req).await.unwrap().accepted);

    let buyer_identity = LocalIdentity::generate("buyer@e2e");
    let http = reqwest::Client::new();
    let sign_demand = |job_id: Uuid, requirement: CapabilityRequirement, idem: &str| {
        let payload = JobEnvelopePayload {
            job_id,
            buyer: buyer_identity.agent_id(),
            kind: JobKind::BatchJob,
            capability_requirement: requirement,
            input: vec![Content::text("printf %s hardware-gated")],
            price_micro_usdc: 1_000,
            deadline_ms: 30_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, idem),
            issued_at_ms: epoch_ms(),
            referral_code: None,
            stream: false,
        };
        SignedJobEnvelope::sign(payload, &buyer_identity).unwrap()
    };
    let requirement = |gpu_class: Option<&str>, min_vram_gb: Option<u32>| CapabilityRequirement {
        gpu_class: gpu_class.map(str::to_string),
        min_vram_gb,
        model_id: None,
        kind: JobKind::BatchJob,
        max_duration_secs: 30,
        min_reputation_bps: None,
    };

    // An ask the 24 GB rtx-4090 cannot meet — too much VRAM, or a class it
    // is not — refuses at admission and refunds the hold.
    for (req, idem) in [
        (requirement(None, Some(80)), "e2e-demand-vram"),
        (requirement(Some("h100"), None), "e2e-demand-class"),
    ] {
        let job_id = Uuid::new_v4();
        let envelope = sign_demand(job_id, req, idem);
        let resp = http
            .post(format!("{base_url}/federation/jobs"))
            .json(&envelope)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT, "{idem}");
        assert_eq!(
            state_handle.escrow().status(job_id).await.unwrap(),
            EscrowStatus::Refunded,
            "{idem}: an unmeetable ask refunds the hold, never dispatches"
        );
        assert_eq!(
            state_handle.jobs().get(job_id).unwrap().refund_reason,
            Some(covenant_compute_protocol::RefundReason::AdmissionFailed),
            "{idem}",
        );
    }

    // The exact class and a VRAM floor the operator clears: a demand it CAN
    // serve still lands, so the gate narrows supply without excluding a
    // capable operator.
    let met_id = Uuid::new_v4();
    let envelope = sign_demand(
        met_id,
        requirement(Some("rtx-4090"), Some(16)),
        "e2e-demand-met",
    );
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(
        state_handle.escrow().status(met_id).await.unwrap(),
        EscrowStatus::Held
    );
}

#[tokio::test]
async fn a_marketplace_fee_is_disclosed_split_from_the_payout_and_audited() {
    // 2.5% take. The buyer pays 1_000 gross; the operator nets 975.
    let identity = LocalIdentity::generate("coordinator@e2e");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    let config = CoordinatorConfig {
        long_poll_timeout: Duration::from_secs(5),
        default_funding_source: FundingSource::Organic,
        fee: covenant_compute_protocol::MarketplaceFee::new(250).unwrap(),
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::new(identity, config, reputation, payout.clone(), audit);
    let state_handle = state.clone();
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@e2e");
    let profile = cpu_profile(&operator_identity, 1_000);
    let coordinator_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        2,
    ));

    // The fee is disclosed at registration — the operator prices its
    // ask knowing the take, and a node's earnings math uses this rate.
    let register_req =
        RegisterRequest::sign(profile.clone(), payout_addr(25), &operator_identity).unwrap();
    let register_resp = coordinator_client.register(register_req).await.unwrap();
    assert!(register_resp.accepted);
    assert_eq!(register_resp.fee_bps, 250);

    let earnings = Arc::new(InMemoryEarningsLedger::new());
    let node = Node::new(
        operator_identity,
        profile,
        coordinator_client,
        Arc::new(EchoExecutor),
        earnings.clone(),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: register_resp.fee_bps,
        },
    );

    let buyer_identity = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let envelope = signed_envelope(
        &buyer_identity,
        job_id,
        1_000,
        30_000,
        epoch_ms(),
        "e2e-fee-capture",
    );
    let http = reqwest::Client::new();
    let submit_resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit_resp.status(), reqwest::StatusCode::ACCEPTED);

    node.run_once()
        .await
        .expect("run_once should succeed")
        .expect("the job must have been offered to this operator");

    // The hold released gross — the buyer's price and the revenue
    // books are untouched by the fee.
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released
    );
    assert_eq!(
        state_handle.escrow().hold_info(job_id),
        Some((1_000, FundingSource::Organic))
    );

    // The payout push carried the operator's net, the job record pinned
    // the fee, and both sides' books agree: 975 + 25 = 1_000.
    let records = payout.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].amount_micro_usdc, 975);
    assert_eq!(records[0].payout_address, payout_addr(25));
    assert_eq!(state_handle.jobs().get(job_id).unwrap().fee_micro_usdc, 25);
    assert_eq!(earnings.unpaid_total_micro_usdc().await, 975);
    let credited = earnings.recent(1).await;
    assert_eq!(credited[0].fee_micro_usdc, 25);

    // The take is auditable per job and inspectable in aggregate.
    let coordinator_events = state_handle.audit().recent(10).await.unwrap();
    assert!(coordinator_events.iter().any(|e| matches!(
        &e.kind,
        AuditKind::ComputeFeeCaptured {
            job_id: id,
            fee_bps: 250,
            fee_micro_usdc: 25,
            operator_net_micro_usdc: 975,
            ..
        } if *id == job_id
    )));
    assert!(coordinator_events.iter().any(|e| matches!(
        &e.kind,
        AuditKind::ComputeJobReleased { job_id: id, amount_micro_usdc: 1_000, .. } if *id == job_id
    )));

    let fees: serde_json::Value = http
        .get(format!("{base_url}/federation/fees"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(fees["fee_bps"], 250);
    assert_eq!(fees["captured_micro_usdc"], 25);
    assert_eq!(fees["jobs_charged"], 1);
}

/// Track A discovery over the real wire: `GET /federation/capacity` is
/// the first read a stranger buyer makes — it must parse into the
/// protocol's `CapacityView` (the type the buyer crate, the MCP tool
/// and covenantd all consume), reflect a registration the moment the
/// matcher would, and stop advertising an operator that declares
/// itself Offline. No identities anywhere in the body: an open
/// directory must not be a target list.
#[tokio::test]
async fn capacity_directory_tracks_registrations_and_standing_over_the_wire() {
    let (state, _payout) = new_coordinator_state(Duration::from_millis(200));
    let base_url = spawn_coordinator(state).await;
    let http = reqwest::Client::new();
    let capacity_url = format!("{base_url}/federation/capacity");

    let fetch = |http: reqwest::Client, url: String| async move {
        http.get(&url)
            .send()
            .await
            .unwrap()
            .json::<CapacityView>()
            .await
            .expect("the body is the protocol's CapacityView, verbatim")
    };

    let empty = fetch(http.clone(), capacity_url.clone()).await;
    assert_eq!(empty.registered_operators, 0);
    assert_eq!(empty.matchable_operators, 0);
    assert!(empty.entries.is_empty());

    // A generic exec node and a GPU inference node register over HTTP.
    let exec_identity = LocalIdentity::generate("exec@capacity");
    let register = RegisterRequest::sign(
        cpu_profile(&exec_identity, 1_000),
        payout_addr(26),
        &exec_identity,
    )
    .unwrap();
    let resp = http
        .post(format!("{base_url}/federation/operators/register"))
        .json(&register)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let gpu_identity = LocalIdentity::generate("gpu@capacity");
    let gpu_profile = CapabilityProfile {
        operator: gpu_identity.agent_id(),
        hardware: HardwareClass::ConsumerGpu {
            model: "rtx-4090".into(),
        },
        vram_gb: 24,
        models_served: vec!["qwen2.5-coder:7b".into()],
        job_kinds: vec![JobKind::InferenceCall],
        price: PriceAsk {
            unit: PriceUnit::PerMillionTokens,
            micro_usdc: 400,
        },
        tee_capable: false,
        kind_prices: Vec::new(),
        kind_models: Vec::new(),
    };
    let register = RegisterRequest::sign(gpu_profile, payout_addr(7), &gpu_identity).unwrap();
    let resp = http
        .post(format!("{base_url}/federation/operators/register"))
        .json(&register)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let stocked = fetch(http.clone(), capacity_url.clone()).await;
    assert_eq!(stocked.registered_operators, 2);
    assert_eq!(stocked.matchable_operators, 2);
    let rows: Vec<(JobKind, &str, u64)> = stocked
        .entries
        .iter()
        .map(|e| (e.kind, e.model.as_str(), e.min_ask_micro_usdc))
        .collect();
    assert_eq!(
        rows,
        vec![
            (JobKind::InferenceCall, "qwen2.5-coder:7b", 400),
            (JobKind::BatchJob, "any", 1_000),
        ],
        "each declared profile is one purchasable row with its ask as the floor"
    );

    // The body carries no operator identity, in any spelling.
    let raw = http
        .get(&capacity_url)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    for leak in [
        exec_identity.agent_id().pubkey_base58(),
        gpu_identity.agent_id().pubkey_base58(),
        payout_addr(26),
        payout_addr(7),
    ] {
        assert!(!raw.contains(&leak), "directory leaked {leak}: {raw}");
    }

    // The GPU node signs off; its rows leave the directory with it.
    let bye = covenant_compute_protocol::HeartbeatRequest::sign(
        gpu_identity.agent_id(),
        covenant_compute_protocol::OperatorStatus::Offline,
        0,
        epoch_ms(),
        &gpu_identity,
    )
    .unwrap();
    let resp = http
        .post(format!("{base_url}/federation/operators/heartbeat"))
        .json(&bye)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let drained = fetch(http.clone(), capacity_url.clone()).await;
    assert_eq!(drained.registered_operators, 2, "still registered");
    assert_eq!(drained.matchable_operators, 1, "no longer matchable");
    assert_eq!(
        drained
            .entries
            .iter()
            .map(|e| (e.kind, e.model.as_str()))
            .collect::<Vec<_>>(),
        vec![(JobKind::BatchJob, "any")],
        "an Offline operator's exclusive rows are gone, not advertised-but-unmatchable"
    );
}

/// The floors a deployment enforces are echoed by the directory, each
/// in its own slot — a buyer reading an empty view can then tell "no
/// supply exists" from "supply exists below this deployment's floors".
/// Distinct values pin the handler wiring each config knob to the
/// right field; the default-config test can't (they're all zero).
#[tokio::test]
async fn capacity_directory_echoes_the_floors_in_force() {
    let identity = LocalIdentity::generate("coordinator@e2e");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    let config = CoordinatorConfig {
        min_operator_score_bps: 1_500,
        min_bond_micro_usdc: 250_000,
        operator_liveness_timeout: Duration::from_secs(31),
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::new(identity, config, reputation, payout, audit);
    let base_url = spawn_coordinator(state).await;

    let view: CapacityView = reqwest::get(format!("{base_url}/federation/capacity"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(view.min_score_bps, 1_500);
    assert_eq!(view.min_bond_micro_usdc, 250_000);
    assert_eq!(view.liveness_window_ms, 31_000);
}

/// A buyer sizing a reputation-floored buy passes `?min_reputation_bps=`
/// so the directory aggregates only the pool their job could match — the
/// price they read is the one they would pay. The floor rides all the way
/// to the matcher's own `capacity_view`, echoed back as the floor in
/// force, and an unreachable floor empties the directory rather than
/// advertising supply the buyer's job would refuse. An unfloored read of
/// this public endpoint keeps its old behaviour and runs no reputation
/// scan at all.
#[tokio::test]
async fn capacity_directory_applies_a_buyer_reputation_floor_from_the_query() {
    let (state, _payout) = new_coordinator_state(Duration::from_secs(31));
    let base_url = spawn_coordinator(state).await;
    let http = reqwest::Client::new();
    let capacity_url = format!("{base_url}/federation/capacity");

    for (i, display) in ["op-a@capacity", "op-b@capacity"].into_iter().enumerate() {
        let identity = LocalIdentity::generate(display);
        let register = RegisterRequest::sign(
            cpu_profile(&identity, 1_000),
            payout_addr(40 + i as u8),
            &identity,
        )
        .unwrap();
        let resp = http
            .post(format!("{base_url}/federation/operators/register"))
            .json(&register)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
    }

    let fetch = |url: String| {
        let http = http.clone();
        async move {
            http.get(&url)
                .send()
                .await
                .unwrap()
                .json::<CapacityView>()
                .await
                .unwrap()
        }
    };

    // No floor: both operators advertise, and the coordinator's own floor
    // (zero here) is echoed.
    let open = fetch(capacity_url.clone()).await;
    assert_eq!(open.matchable_operators, 2);
    assert_eq!(open.min_score_bps, 0);
    assert!(!open.entries.is_empty());

    // An unreachable floor (no smoothed score reaches 100%) empties the
    // directory, and the floor the buyer asked for is the one echoed.
    let floored = fetch(format!("{capacity_url}?min_reputation_bps=10000")).await;
    assert_eq!(floored.min_score_bps, 10_000);
    assert_eq!(
        floored.matchable_operators, 0,
        "a floor no operator clears leaves nothing to advertise"
    );
    assert!(floored.entries.is_empty());
    // The registered count is the whole pool, floor or not.
    assert_eq!(floored.registered_operators, 2);
}

/// C8 at settlement, both sides through real HTTP: the accrual fields
/// the partner books trust are WRITTEN by the release path, not seeded.
/// Pins the split order the fee can never be outrun by — the supply
/// share comes off the whole captured fee, the buyer's partner earns
/// from what remains — plus the same-partner-both-sides job counting
/// once in the public view, and an unconfigured buyer code accruing
/// nothing and writing no row.
#[tokio::test]
async fn rev_shares_accrue_from_real_settlements_and_the_buyer_share_takes_the_fee_remainder() {
    use covenant_compute_coordinator::PartnerConfig;

    let identity = LocalIdentity::generate("coordinator@e2e");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    // Both partners at 60% of the fee: on any shared job the two raw
    // shares (120 + 120 of a 200 fee) would overdraw the take — the
    // remainder clamp is what this configuration forces to bite.
    let mut partners = std::collections::HashMap::new();
    partners.insert(
        "partner-s".to_string(),
        PartnerConfig::new("partner-s-address".into(), 6_000).unwrap(),
    );
    partners.insert(
        "partner-b".to_string(),
        PartnerConfig::new("partner-b-address".into(), 6_000).unwrap(),
    );
    let config = CoordinatorConfig {
        long_poll_timeout: Duration::from_secs(5),
        default_funding_source: FundingSource::Organic,
        fee: covenant_compute_protocol::MarketplaceFee::new(2_000).unwrap(),
        partners,
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::new(identity, config, reputation, payout.clone(), audit);
    let state_handle = state.clone();
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state).await;

    // The operator registers with a signed supply-side attribution, so
    // every job it serves accrues partner-s's cut of the fee.
    let operator_identity = LocalIdentity::generate("operator@e2e");
    let profile = cpu_profile(&operator_identity, 1_000);
    let coordinator_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        2,
    ));
    let register_req = RegisterRequest::sign_referred(
        profile.clone(),
        payout_addr(6),
        Some("partner-s".into()),
        &operator_identity,
    )
    .unwrap();
    let register_resp = coordinator_client.register(register_req).await.unwrap();
    assert!(register_resp.accepted);
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
            fee_bps: register_resp.fee_bps,
        },
    );
    let http = reqwest::Client::new();
    let buy = |envelope: SignedJobEnvelope| {
        let http = http.clone();
        let base_url = base_url.clone();
        async move {
            let resp = http
                .post(format!("{base_url}/federation/jobs"))
                .json(&envelope)
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
        }
    };

    // Job 1, partners on both sides: 1_000 gross, 200 fee, 800 net.
    // Supply takes 60% of the whole fee (120); the buyer's partner is
    // owed 120 too but only 80 remains — the clamp, not the raw share.
    let job_one = Uuid::new_v4();
    buy(referred_envelope(
        &LocalIdentity::generate("buyer-one@e2e"),
        job_one,
        1_000,
        "rev-share-two-sided",
        "partner-b",
    ))
    .await;
    node.run_once()
        .await
        .expect("run_once should succeed")
        .expect("job one must have been offered to this operator");

    let record = state_handle.jobs().get(job_one).unwrap();
    assert_eq!(record.fee_micro_usdc, 200);
    assert_eq!(record.partner_share_micro_usdc, 120);
    assert_eq!(
        record.buyer_partner_share_micro_usdc, 80,
        "the buyer's partner earns the remainder of the fee, not its raw share"
    );
    assert_eq!(
        payout.records()[0].amount_micro_usdc,
        800,
        "operator net is untouched by rev-shares"
    );

    let events = state_handle.audit().recent(50).await.unwrap();
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        AuditKind::ComputePartnerShareAccrued {
            job_id,
            referral_code,
            partner_payout_address,
            share_micro_usdc: 120,
            fee_micro_usdc: 200,
        } if *job_id == job_one && referral_code == "partner-s" && partner_payout_address == "partner-s-address"
    )));
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        AuditKind::ComputeBuyerPartnerShareAccrued {
            job_id,
            referral_code,
            partner_payout_address,
            share_micro_usdc: 80,
            fee_micro_usdc: 200,
        } if *job_id == job_one && referral_code == "partner-b" && partner_payout_address == "partner-b-address"
    )));

    // Job 2, the same partner on both sides: both cuts accrue to one
    // code (120 supply + 80 remainder), and the view below counts the
    // job once, not twice.
    let job_two = Uuid::new_v4();
    buy(referred_envelope(
        &LocalIdentity::generate("buyer-two@e2e"),
        job_two,
        1_000,
        "rev-share-same-code",
        "partner-s",
    ))
    .await;
    node.run_once()
        .await
        .expect("run_once should succeed")
        .expect("job two must have been offered to this operator");
    let record = state_handle.jobs().get(job_two).unwrap();
    assert_eq!(
        (
            record.partner_share_micro_usdc,
            record.buyer_partner_share_micro_usdc
        ),
        (120, 80)
    );

    // Job 3, a buyer code nobody configured: attribution stays on the
    // record, but nothing accrues and no accrual row is written.
    let job_three = Uuid::new_v4();
    buy(referred_envelope(
        &LocalIdentity::generate("buyer-three@e2e"),
        job_three,
        1_000,
        "rev-share-unknown-code",
        "code-nobody",
    ))
    .await;
    node.run_once()
        .await
        .expect("run_once should succeed")
        .expect("job three must have been offered to this operator");
    let record = state_handle.jobs().get(job_three).unwrap();
    assert_eq!(record.buyer_referral_code.as_deref(), Some("code-nobody"));
    assert_eq!(record.buyer_partner_share_micro_usdc, 0);
    let events = state_handle.audit().recent(50).await.unwrap();
    assert!(
        !events.iter().any(|e| matches!(
            &e.kind,
            AuditKind::ComputeBuyerPartnerShareAccrued { job_id, .. } if *job_id == job_three
        )),
        "an unconfigured code accrues nothing"
    );

    // No settled job's shares ever sum past its captured fee.
    for job_id in [job_one, job_two, job_three] {
        let r = state_handle.jobs().get(job_id).unwrap();
        assert!(
            r.partner_share_micro_usdc + r.buyer_partner_share_micro_usdc <= r.fee_micro_usdc,
            "job {job_id}: shares outran the fee"
        );
    }

    // The public books agree: both sides summed per code, the
    // two-sided job attributed once, the unknown code absent.
    let partners_view: Vec<serde_json::Value> = http
        .get(format!("{base_url}/federation/partners"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = |code: &str| {
        partners_view
            .iter()
            .find(|p| p["referral_code"] == code)
            .unwrap_or_else(|| panic!("no partners row for {code}"))
            .clone()
    };
    let supply = row("partner-s");
    assert_eq!(supply["accrued_micro_usdc"], 440, "120 + (120 + 80) + 120");
    assert_eq!(supply["jobs_attributed"], 3);
    let demand = row("partner-b");
    assert_eq!(demand["accrued_micro_usdc"], 80);
    assert_eq!(demand["jobs_attributed"], 1);
    assert!(
        !partners_view
            .iter()
            .any(|p| p["referral_code"] == "code-nobody"),
        "an unconfigured code with no accruals has no books row"
    );
}

/// C5: the canary prober buys real work from two live nodes through
/// the ordinary pipeline — an instruction-following one and an
/// input-echoing one — pays BOTH (their receipts verify; the fraud is
/// in the content), and lands the verdicts where the matcher can see
/// them: the echo node eats a canary fault, the honest one a clean
/// probe record.
#[tokio::test]
async fn canary_probes_judge_real_nodes_and_feed_reputation() {
    use covenant_compute_coordinator::{CanaryConfig, CanaryProber, SubsidyPolicy};
    use covenant_compute_node::{ExecutionOutcome, ExecutorError, JobExecutor};

    // `honest: true` follows the canary's instruction — a stand-in for
    // a real instruct model. `honest: false` reflects the input back,
    // the cheapest receipt-valid fraud (what `EchoExecutor` does).
    struct CanaryTestExecutor {
        honest: bool,
    }

    #[async_trait::async_trait]
    impl JobExecutor for CanaryTestExecutor {
        async fn execute(
            &self,
            job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<ExecutionOutcome, ExecutorError> {
            let Some(Content::Text { text }) = job.input.first() else {
                return Err(ExecutorError::Failed("no text input".into()));
            };
            let output = if self.honest {
                text.rsplit(' ').next().unwrap_or_default().to_string()
            } else {
                text.clone()
            };
            Ok(ExecutionOutcome {
                output: vec![Content::text(output)],
                wall_ms: 1,
                tokens_in: Some(1),
                tokens_out: Some(1),
                finish_reason: None,
            })
        }
    }

    let identity = LocalIdentity::generate("coordinator@e2e");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    let config = CoordinatorConfig {
        long_poll_timeout: Duration::from_secs(2),
        // Probes are bootstrap-tagged; the floor is what admits them
        // with zero organic revenue on the books.
        subsidy_policy: Some(SubsidyPolicy::new(10_000, 1_000_000).unwrap()),
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::new(identity, config, reputation, payout.clone(), audit);
    let state_handle = state.clone();
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state.clone()).await;

    let mut operators = std::collections::HashMap::new();
    for (display, honest) in [("honest@e2e", true), ("echo@e2e", false)] {
        let operator_identity = LocalIdentity::generate(display);
        let profile = CapabilityProfile {
            models_served: vec!["canary-model".into()],
            job_kinds: vec![JobKind::InferenceCall],
            ..cpu_profile(&operator_identity, 1_000)
        };
        let client = Arc::new(HttpCoordinatorClient::with_config(
            base_url.clone(),
            Duration::from_secs(5),
            2,
        ));
        let register_req =
            RegisterRequest::sign(profile.clone(), payout_for(display), &operator_identity)
                .unwrap();
        client.register(register_req).await.unwrap();
        operators.insert(operator_identity.agent_id().pubkey_base58(), honest);

        let node = Node::new(
            operator_identity,
            profile,
            client,
            Arc::new(CanaryTestExecutor { honest }),
            Arc::new(InMemoryEarningsLedger::new()),
            Arc::new(InMemoryAuditLog::new()),
            NodeConfig {
                coordinator_pubkey_b58: coordinator_pubkey_b58.clone(),
                max_in_flight: 4,
                preempt_grace: Duration::from_secs(2),
                fee_bps: 0,
            },
        );
        tokio::spawn(async move {
            loop {
                let _ = node.run_once().await;
            }
        });
    }

    let prober = CanaryProber::new(
        state.clone(),
        CanaryConfig {
            max_price_micro_usdc: 10_000,
            deadline_ms: 30_000,
        },
    );

    // Each tick dispatches at most one probe and judges whatever
    // finished; drive ticks until both operators have a verdict.
    let mut verdicts: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    let mut probe_ids: Vec<Uuid> = Vec::new();
    let mut judged_ids: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    for _ in 0..60 {
        let report = prober.tick().await;
        if let Some((job_id, _)) = report.dispatched {
            probe_ids.push(job_id);
        }
        for (job_id, operator, passed) in report.judged {
            judged_ids.insert(job_id);
            verdicts.insert(operator, passed);
        }
        if verdicts.len() == operators.len() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        verdicts.len(),
        2,
        "both probes must be judged: {verdicts:?}"
    );
    for (operator, honest) in &operators {
        assert_eq!(
            verdicts.get(operator),
            Some(honest),
            "the honest node passes, the echo node fails"
        );
    }

    // Both operators were PAID — a canary is a real job, and both
    // receipts hash-verified. The echo node's problem is its record.
    let paid: std::collections::HashSet<String> = payout
        .records()
        .iter()
        .map(|r| r.operator_pubkey_b58.clone())
        .collect();
    assert_eq!(paid.len(), 2, "both probes released and paid");

    // Probe spend rode the subsidy books.
    let subsidy = state_handle.escrow().subsidy_status();
    assert!(
        subsidy.bootstrap_committed_micro_usdc >= 2_000,
        "two 1000-micro-USDC probes committed as bootstrap, got {}",
        subsidy.bootstrap_committed_micro_usdc
    );

    // Reputation: the failed canary is a fault the matcher now sees;
    // the passed one is probe history without double-counting.
    for (operator, honest) in &operators {
        let stats = state_handle.reputation().stats(operator).await;
        if *honest {
            assert_eq!((stats.canary_passed, stats.canary_failed), (1, 0));
            assert_eq!(stats.faults, 0);
            assert_eq!(stats.released, 1);
        } else {
            assert_eq!((stats.canary_passed, stats.canary_failed), (0, 1));
            assert_eq!(stats.faults, 1);
            assert_eq!(stats.released, 1, "paid, and still a fault");
        }
    }

    // Fingerprint rotation: EVERY probe was signed by its own freshly
    // generated buyer key wearing the stock MCP buyer's name, and each
    // instruction came from the template pool with the nonce as its
    // final token. Nothing stable is left to allowlist.
    assert!(probe_ids.len() >= 2, "at least one probe per operator");
    let probe_buyers: std::collections::HashSet<String> = probe_ids
        .iter()
        .map(|job_id| {
            let record = state_handle.jobs().get(*job_id).unwrap();
            let buyer = &record.envelope.payload.buyer;
            assert_eq!(buyer.display, "buyer@compute");
            let Content::Text { text } = &record.envelope.payload.input[0] else {
                panic!("canary input is a text block");
            };
            let nonce = text.rsplit(' ').next().unwrap();
            let instruction = text[..text.len() - nonce.len()].trim();
            assert!(
                covenant_compute_coordinator::INFER_TEMPLATES.contains(&instruction),
                "instruction must come from the pool, got: {instruction}"
            );
            buyer.pubkey_base58()
        })
        .collect();
    assert_eq!(
        probe_buyers.len(),
        probe_ids.len(),
        "buyer identity rotates per probe"
    );

    // A second judge pass over the same terminal jobs must not
    // double-book: the resolved set survives within the prober, and a
    // FRESH prober (a restart) rebuilds it from the audit log's
    // dispatch markers and verdicts.
    let restarted = CanaryProber::new(
        state.clone(),
        CanaryConfig {
            max_price_micro_usdc: 10_000,
            deadline_ms: 30_000,
        },
    );
    // The restarted prober may legitimately judge a probe still in
    // flight from the loop above — but it must never RE-judge one that
    // already has a verdict. Once an operator idles it also dispatches
    // a fresh probe, which a THIRD prober (another restart) must find
    // and judge purely from the audit markers, since no stable canary
    // identity exists to recognize it by.
    let mut restart_dispatch = None;
    for _ in 0..30 {
        let report = restarted.tick().await;
        assert!(
            report
                .judged
                .iter()
                .all(|(job_id, _, _)| !judged_ids.contains(job_id)),
            "verdicts are written once, restarts included"
        );
        if report.dispatched.is_some() {
            restart_dispatch = report.dispatched;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (third_probe, third_operator) =
        restart_dispatch.expect("an operator eventually idles and gets probed");
    for _ in 0..100 {
        let phase = state_handle.jobs().get(third_probe).unwrap().phase;
        if phase == covenant_compute_coordinator::JobPhase::Completed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let judge_only = CanaryProber::new(
        state.clone(),
        CanaryConfig {
            max_price_micro_usdc: 10_000,
            deadline_ms: 30_000,
        },
    );
    let mut third_verdict = None;
    for _ in 0..30 {
        let report = judge_only.tick().await;
        if let Some(v) = report
            .judged
            .iter()
            .find(|(job_id, _, _)| *job_id == third_probe)
        {
            third_verdict = Some(v.2);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        third_verdict,
        Some(*operators.get(&third_operator).unwrap()),
        "a fresh prober judges a probe it never dispatched, from the audit markers alone"
    );
}

/// The anti-faucet inheritance: canary spend is bootstrap-tagged, so a
/// coordinator with the subsidy kill-switch closed (no policy) probes
/// nothing at all — the hold is refused before any offer exists.
#[tokio::test]
async fn canary_probes_stop_dead_without_a_subsidy_policy() {
    use covenant_compute_coordinator::{CanaryConfig, CanaryProber};

    let (state, _payout) = new_coordinator_state(Duration::from_secs(2));
    let operator_identity = LocalIdentity::generate("operator@e2e");
    let profile = CapabilityProfile {
        models_served: vec!["canary-model".into()],
        job_kinds: vec![JobKind::InferenceCall],
        ..cpu_profile(&operator_identity, 1_000)
    };
    let register_req = RegisterRequest::sign(profile, payout_addr(1), &operator_identity).unwrap();
    state
        .registry()
        .register(&register_req, epoch_ms(), None, false)
        .unwrap();

    let operator_key = operator_identity.agent_id().pubkey_base58();
    let prober = CanaryProber::new(state.clone(), CanaryConfig::default());
    let report = prober.tick().await;
    assert!(report.dispatched.is_none());
    let reason = report.idle_reason.expect("an idle reason is reported");
    assert!(reason.contains("hold refused"), "got: {reason}");
    // Nothing held, nothing recorded, nothing offered.
    assert!(state.jobs().by_operator(&operator_key).is_empty());
}

/// C5's second half: a released batch job is re-bought from other
/// operators through the ordinary paid pipeline and the receipt hashes
/// are compared. The strict-majority minority is faulted, agreement is
/// recorded, mirror money rides the subsidy books, and a fresh sampler
/// (a restart) re-judges nothing and never samples a mirror — all
/// purely from the audit chain's own markers.
#[tokio::test]
async fn redundancy_sampling_faults_the_hash_minority_and_survives_restart() {
    use covenant_compute_coordinator::{RedundancyConfig, RedundancySampler, SubsidyPolicy};
    use covenant_compute_node::{ExecutionOutcome, ExecutorError, JobExecutor};

    // `honest: true` behaves like a real batch runner: `echo <token>`
    // produces the token. `honest: false` reflects the input back —
    // a receipt-valid answer whose bytes differ from everyone else's,
    // exactly the fraud class hash comparison exists to catch.
    struct BatchTestExecutor {
        honest: bool,
    }

    #[async_trait::async_trait]
    impl JobExecutor for BatchTestExecutor {
        async fn execute(
            &self,
            job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<ExecutionOutcome, ExecutorError> {
            let Some(Content::Text { text }) = job.input.first() else {
                return Err(ExecutorError::Failed("no text input".into()));
            };
            let output = if self.honest {
                text.strip_prefix("echo ").unwrap_or(text).to_string()
            } else {
                text.clone()
            };
            Ok(ExecutionOutcome {
                output: vec![Content::text(output)],
                wall_ms: 1,
                tokens_in: None,
                tokens_out: None,
                finish_reason: None,
            })
        }
    }

    let identity = LocalIdentity::generate("coordinator@e2e");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    let config = CoordinatorConfig {
        long_poll_timeout: Duration::from_secs(2),
        // Mirrors are bootstrap-tagged; the floor admits them with no
        // organic revenue on the books yet.
        subsidy_policy: Some(SubsidyPolicy::new(10_000, 1_000_000).unwrap()),
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::new(identity, config, reputation, payout.clone(), audit.clone());
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state.clone()).await;

    // The source operator asks cheapest, so it wins the organic match;
    // the other two only ever see mirror traffic.
    let mut operators: std::collections::HashMap<String, (&str, bool)> =
        std::collections::HashMap::new();
    for (display, ask, honest) in [
        ("source@e2e", 800u64, true),
        ("mirror-honest@e2e", 1_000, true),
        ("mirror-echo@e2e", 1_000, false),
    ] {
        let operator_identity = LocalIdentity::generate(display);
        let profile = cpu_profile(&operator_identity, ask);
        let client = Arc::new(HttpCoordinatorClient::with_config(
            base_url.clone(),
            Duration::from_secs(5),
            2,
        ));
        let register_req =
            RegisterRequest::sign(profile.clone(), payout_for(display), &operator_identity)
                .unwrap();
        client.register(register_req).await.unwrap();
        operators.insert(
            operator_identity.agent_id().pubkey_base58(),
            (display, honest),
        );

        let node = Node::new(
            operator_identity,
            profile,
            client,
            Arc::new(BatchTestExecutor { honest }),
            Arc::new(InMemoryEarningsLedger::new()),
            Arc::new(InMemoryAuditLog::new()),
            NodeConfig {
                coordinator_pubkey_b58: coordinator_pubkey_b58.clone(),
                max_in_flight: 4,
                preempt_grace: Duration::from_secs(2),
                fee_bps: 0,
            },
        );
        tokio::spawn(async move {
            loop {
                let _ = node.run_once().await;
            }
        });
    }
    let display_of = |pubkey: &str| operators.get(pubkey).map(|(d, _)| *d).unwrap_or("?");

    // A real buyer's batch job through the ordinary submit path.
    let buyer = LocalIdentity::generate("buyer@e2e");
    let http = reqwest::Client::new();
    let submit_batch = |job_id: Uuid, input: &'static str, idem: &'static str| {
        let buyer = &buyer;
        let http = &http;
        let base_url = &base_url;
        async move {
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
                input: vec![Content::text(input)],
                price_micro_usdc: 1_000,
                deadline_ms: 30_000,
                idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, idem),
                issued_at_ms: epoch_ms(),
                referral_code: None,
                stream: false,
            };
            let envelope = SignedJobEnvelope::sign(payload, buyer).unwrap();
            let submit = http
                .post(format!("{base_url}/federation/jobs"))
                .json(&envelope)
                .send()
                .await
                .unwrap();
            assert_eq!(submit.status(), reqwest::StatusCode::ACCEPTED);
        }
    };
    let source_job = Uuid::new_v4();
    submit_batch(source_job, "echo redundancy-e2e-token", "redundancy-1").await;
    for _ in 0..100 {
        if state.jobs().get(source_job).unwrap().phase
            == covenant_compute_coordinator::JobPhase::Completed
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let source_record = state.jobs().get(source_job).unwrap();
    assert_eq!(
        source_record.phase,
        covenant_compute_coordinator::JobPhase::Completed
    );
    assert_eq!(display_of(&source_record.operator_pubkey_b58), "source@e2e");

    // Sample it: two mirrors, judged once every mirror concludes.
    let sampler = RedundancySampler::new(state.clone(), RedundancyConfig::default());
    let mut mirror_ids: Vec<(Uuid, String)> = Vec::new();
    let mut judged: Vec<covenant_compute_coordinator::SampleVerdict> = Vec::new();
    for _ in 0..100 {
        let report = sampler.tick().await;
        if let Some((sampled_source, mirrors)) = report.dispatched {
            assert_eq!(sampled_source, source_job, "the one released batch job");
            mirror_ids = mirrors;
        }
        judged.extend(report.judged);
        if !judged.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        mirror_ids.len(),
        2,
        "both other operators drew a mirror: {mirror_ids:?}"
    );
    assert_eq!(judged.len(), 3, "one verdict per participating receipt");
    for verdict in &judged {
        assert_eq!(verdict.source_job_id, source_job);
        let honest = operators
            .get(&verdict.operator_pubkey_b58)
            .map(|(_, honest)| *honest)
            .unwrap();
        assert_eq!(
            verdict.agreed,
            Some(honest),
            "honest hashes agree, the echo node is the minority: {verdict:?}"
        );
    }

    // The mirrors are real paid jobs wearing ordinary buyer clothes:
    // fresh buyer key each, the stock MCP buyer display name, the
    // source's exact input — and their spend rode the subsidy books.
    let mut mirror_buyers = std::collections::HashSet::new();
    for (mirror_id, _) in &mirror_ids {
        let record = state.jobs().get(*mirror_id).unwrap();
        assert_eq!(record.envelope.payload.buyer.display, "buyer@compute");
        assert_eq!(
            record.envelope.payload.input, source_record.envelope.payload.input,
            "a mirror re-buys the source input verbatim"
        );
        mirror_buyers.insert(record.envelope.payload.buyer.pubkey_base58());
    }
    assert_eq!(mirror_buyers.len(), 2, "buyer identity rotates per mirror");
    assert!(
        state
            .escrow()
            .subsidy_status()
            .bootstrap_committed_micro_usdc
            >= 2_000,
        "two 1000-micro mirrors committed as bootstrap"
    );
    let paid: std::collections::HashSet<String> = payout
        .records()
        .iter()
        .map(|r| r.operator_pubkey_b58.clone())
        .collect();
    assert_eq!(paid.len(), 3, "the source release and both mirrors paid");

    // Reputation: the disagreeing operator carries the fault, everyone
    // else carries the informational agreement.
    for (pubkey, (display, honest)) in &operators {
        let stats = state.reputation().stats(pubkey).await;
        if *honest {
            assert_eq!(
                (
                    stats.redundancy_agreed,
                    stats.redundancy_disagreed,
                    stats.faults
                ),
                (1, 0, 0),
                "{display}"
            );
        } else {
            assert_eq!(
                (
                    stats.redundancy_agreed,
                    stats.redundancy_disagreed,
                    stats.faults
                ),
                (0, 1, 1),
                "{display}"
            );
            assert_eq!(stats.released, 1, "paid for the mirror, and still a fault");
        }
    }

    // A fresh sampler (a restart) rebuilds its books from the audit
    // chain: nothing is re-judged, and the completed MIRROR jobs —
    // batch jobs with receipts, to a naive scan — are never sampled.
    // Only new organic work draws the next sample.
    let second_job = Uuid::new_v4();
    submit_batch(second_job, "echo redundancy-e2e-second", "redundancy-2").await;
    let restarted = RedundancySampler::new(state.clone(), RedundancyConfig::default());
    let mut second_judged = Vec::new();
    for _ in 0..100 {
        let report = restarted.tick().await;
        if let Some((sampled_source, _)) = &report.dispatched {
            assert_eq!(
                *sampled_source, second_job,
                "a restart samples the new job, never a mirror or a sampled source"
            );
        }
        second_judged.extend(report.judged);
        if !second_judged.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        second_judged.iter().all(|v| v.source_job_id == second_job),
        "verdicts are written once, restarts included: {second_judged:?}"
    );

    // The audit chain agrees end to end: two samples, two mirrors
    // each, three verdict rows each, and no mirror ever a source.
    let events = audit.recent(usize::MAX).await.unwrap();
    let mut dispatched_rows = 0usize;
    let mut result_rows = 0usize;
    let mirror_id_set: std::collections::HashSet<Uuid> =
        mirror_ids.iter().map(|(id, _)| *id).collect();
    for event in &events {
        match &event.kind {
            AuditKind::ComputeRedundancyDispatched { source_job_id, .. } => {
                dispatched_rows += 1;
                assert!(!mirror_id_set.contains(source_job_id));
            }
            AuditKind::ComputeRedundancyResult { .. } => result_rows += 1,
            _ => {}
        }
    }
    assert_eq!(dispatched_rows, 4);
    assert_eq!(result_rows, 6);
}

/// Shared rig for the partial-verdict-write recovery tests: three
/// operators (a dishonest source asking cheapest, two honest mirrors),
/// one released batch job, mirrors dispatched by a first sampler and
/// run to completion — stopped exactly at the point where judging
/// would begin, so each test can stage its own crashed verdict state
/// before a "restarted" sampler takes over.
async fn partial_verdict_rig() -> (
    CoordinatorState,
    Arc<dyn AuditLog>,
    Uuid,
    String,
    Vec<(Uuid, String)>,
) {
    use covenant_compute_coordinator::{RedundancyConfig, RedundancySampler, SubsidyPolicy};
    use covenant_compute_node::{ExecutionOutcome, ExecutorError, JobExecutor};

    struct BatchTestExecutor {
        honest: bool,
    }

    #[async_trait::async_trait]
    impl JobExecutor for BatchTestExecutor {
        async fn execute(
            &self,
            job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<ExecutionOutcome, ExecutorError> {
            let Some(Content::Text { text }) = job.input.first() else {
                return Err(ExecutorError::Failed("no text input".into()));
            };
            let output = if self.honest {
                text.strip_prefix("echo ").unwrap_or(text).to_string()
            } else {
                text.clone()
            };
            Ok(ExecutionOutcome {
                output: vec![Content::text(output)],
                wall_ms: 1,
                tokens_in: None,
                tokens_out: None,
                finish_reason: None,
            })
        }
    }

    let identity = LocalIdentity::generate("coordinator@e2e");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    let config = CoordinatorConfig {
        long_poll_timeout: Duration::from_secs(2),
        subsidy_policy: Some(SubsidyPolicy::new(10_000, 1_000_000).unwrap()),
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::new(identity, config, reputation, payout, audit.clone());
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state.clone()).await;

    // The DISHONEST operator asks cheapest and wins the organic match;
    // the honest pair only ever serve mirrors — so the dropped verdict
    // is the source's own fault row, the exact row the crash loses.
    for (display, ask, honest) in [
        ("bad-source@e2e", 800u64, false),
        ("mirror-a@e2e", 1_000, true),
        ("mirror-b@e2e", 1_000, true),
    ] {
        let operator_identity = LocalIdentity::generate(display);
        let profile = cpu_profile(&operator_identity, ask);
        let client = Arc::new(HttpCoordinatorClient::with_config(
            base_url.clone(),
            Duration::from_secs(5),
            2,
        ));
        let register_req =
            RegisterRequest::sign(profile.clone(), payout_for(display), &operator_identity)
                .unwrap();
        client.register(register_req).await.unwrap();
        let node = Node::new(
            operator_identity,
            profile,
            client,
            Arc::new(BatchTestExecutor { honest }),
            Arc::new(InMemoryEarningsLedger::new()),
            Arc::new(InMemoryAuditLog::new()),
            NodeConfig {
                coordinator_pubkey_b58: coordinator_pubkey_b58.clone(),
                max_in_flight: 4,
                preempt_grace: Duration::from_secs(2),
                fee_bps: 0,
            },
        );
        tokio::spawn(async move {
            loop {
                let _ = node.run_once().await;
            }
        });
    }

    let buyer = LocalIdentity::generate("buyer@e2e");
    let http = reqwest::Client::new();
    let source_job = Uuid::new_v4();
    let payload = JobEnvelopePayload {
        job_id: source_job,
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
        input: vec![Content::text("echo crash-window-token")],
        price_micro_usdc: 1_000,
        deadline_ms: 30_000,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "crash-window-1"),
        issued_at_ms: epoch_ms(),
        referral_code: None,
        stream: false,
    };
    let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
    let submit = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit.status(), reqwest::StatusCode::ACCEPTED);
    for _ in 0..100 {
        if state.jobs().get(source_job).unwrap().phase
            == covenant_compute_coordinator::JobPhase::Completed
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let source_operator = state
        .jobs()
        .get(source_job)
        .unwrap()
        .operator_pubkey_b58
        .clone();

    // First sampler: dispatch only. It is never ticked again — as far
    // as the books are concerned, the process died before judging.
    let sampler = RedundancySampler::new(state.clone(), RedundancyConfig::default());
    let mut mirror_ids: Vec<(Uuid, String)> = Vec::new();
    for _ in 0..100 {
        let report = sampler.tick().await;
        assert!(
            report.judged.is_empty(),
            "mirrors must still be in flight on the dispatch tick"
        );
        if let Some((_, mirrors)) = report.dispatched {
            mirror_ids = mirrors;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(mirror_ids.len(), 2);
    for _ in 0..100 {
        let done = mirror_ids.iter().all(|(id, _)| {
            state.jobs().get(*id).map(|r| r.phase)
                == Some(covenant_compute_coordinator::JobPhase::Completed)
        });
        if done {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Stake for the slash the recovered fault must take.
    state
        .bonds()
        .credit_post("bond-crash-window", &source_operator, 5_000)
        .unwrap();

    (state, audit, source_job, source_operator, mirror_ids)
}

/// Writes the verdict row the crashed pass supposedly got to — byte
/// shape identical to the sampler's own rows, since the books are
/// seeded from the audit chain alone.
async fn plant_verdict_row(
    state: &CoordinatorState,
    audit: &Arc<dyn AuditLog>,
    source_job_id: Uuid,
    operator_pubkey_b58: &str,
    agreed: Option<bool>,
) {
    audit
        .record(covenant_audit::AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: epoch_ms(),
            issuer: state.coordinator_agent_id(),
            kind: AuditKind::ComputeRedundancyResult {
                source_job_id,
                operator_pubkey_b58: operator_pubkey_b58.to_string(),
                agreed,
                detail: "planted by the crash-window test".into(),
            },
        })
        .await
        .unwrap();
}

/// The trust-durability drop from the adversarial pass: verdict rows
/// write mirrors-first, source-last, and a crash after the first
/// mirror row used to mark the source resolved forever — its fault and
/// slash permanently gone. A restarted sampler must instead complete
/// the set: write only the missing rows, fault the source, take the
/// slash, and never double-count the row that did land.
#[tokio::test]
async fn a_crash_mid_verdict_set_re_judges_to_completion_without_double_counting() {
    use covenant_compute_coordinator::{RedundancyConfig, RedundancySampler};

    let (state, audit, source_job, source_operator, mirror_ids) = partial_verdict_rig().await;
    let (_, first_mirror_operator) = &mirror_ids[0];
    plant_verdict_row(
        &state,
        &audit,
        source_job,
        first_mirror_operator,
        Some(true),
    )
    .await;

    // The "restart": a fresh sampler seeded only from the audit chain.
    let restarted = RedundancySampler::new(state.clone(), RedundancyConfig::default());
    let mut judged = Vec::new();
    for _ in 0..100 {
        judged.extend(restarted.tick().await.judged);
        if !judged.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        judged.len(),
        2,
        "exactly the two missing rows are written, never the planted one again: {judged:?}"
    );
    assert!(judged
        .iter()
        .any(|v| v.operator_pubkey_b58 == source_operator && v.agreed == Some(false)));

    // One row per participant, no double-count anywhere.
    let events = audit.recent(usize::MAX).await.unwrap();
    let mut per_operator: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for event in &events {
        if let AuditKind::ComputeRedundancyResult {
            source_job_id,
            operator_pubkey_b58,
            ..
        } = &event.kind
        {
            if *source_job_id == source_job {
                *per_operator.entry(operator_pubkey_b58.clone()).or_default() += 1;
            }
        }
    }
    assert_eq!(per_operator.len(), 3);
    assert!(
        per_operator.values().all(|&count| count == 1),
        "every participant has exactly one verdict row: {per_operator:?}"
    );

    // The recovered fault took its slash.
    let bond = state.bonds().status(&source_operator);
    assert_eq!(
        bond.slashed_micro_usdc, 1_000,
        "the source's fault slashes the sampled job's price"
    );

    // A second restart re-judges to all-skipped: no new rows, no
    // second slash.
    let again = RedundancySampler::new(state.clone(), RedundancyConfig::default());
    let report = again.tick().await;
    assert!(
        report.judged.iter().all(|v| v.source_job_id != source_job),
        "a fully-judged source writes nothing on re-judge: {report:?}"
    );
    let rows_after: usize = audit
        .recent(usize::MAX)
        .await
        .unwrap()
        .iter()
        .filter(|e| {
            matches!(
                &e.kind,
                AuditKind::ComputeRedundancyResult { source_job_id, .. }
                    if *source_job_id == source_job
            )
        })
        .count();
    assert_eq!(rows_after, 3);
    assert_eq!(
        state.bonds().status(&source_operator).slashed_micro_usdc,
        1_000
    );
}

/// The narrower sub-window: the source's fault row landed but the
/// process died before `slash_for_fault`. The written-pair skip must
/// not skip the SLASH too — a re-judge re-fires it (idempotent by
/// slash id), so the one operator redundancy exists to catch can't
/// keep their stake by crashing the coordinator at the right moment.
#[tokio::test]
async fn a_verdict_row_that_landed_without_its_slash_still_slashes_on_re_judge() {
    use covenant_compute_coordinator::{RedundancyConfig, RedundancySampler};

    let (state, audit, source_job, source_operator, mirror_ids) = partial_verdict_rig().await;
    for (_, mirror_operator) in &mirror_ids {
        plant_verdict_row(&state, &audit, source_job, mirror_operator, Some(true)).await;
    }
    plant_verdict_row(&state, &audit, source_job, &source_operator, Some(false)).await;
    assert_eq!(state.bonds().status(&source_operator).slashed_micro_usdc, 0);

    let restarted = RedundancySampler::new(state.clone(), RedundancyConfig::default());
    let mut ticks = 0;
    while state.bonds().status(&source_operator).slashed_micro_usdc == 0 && ticks < 100 {
        let report = restarted.tick().await;
        assert!(
            report.judged.is_empty(),
            "every row is already on disk; only the slash re-fires: {report:?}"
        );
        ticks += 1;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        state.bonds().status(&source_operator).slashed_micro_usdc,
        1_000,
        "the dropped slash lands on re-judge"
    );

    let rows: usize = audit
        .recent(usize::MAX)
        .await
        .unwrap()
        .iter()
        .filter(|e| {
            matches!(
                &e.kind,
                AuditKind::ComputeRedundancyResult { source_job_id, .. }
                    if *source_job_id == source_job
            )
        })
        .count();
    assert_eq!(rows, 3, "no planted row is ever re-written");
}

/// A mirror is a measuring instrument: the matcher's trust floor keeps
/// proven-bad operators out of the mirror pool even when they ask the
/// cheapest — unlike canary probes, which deliberately target floored
/// operators as their road back up.
#[tokio::test]
async fn redundancy_mirrors_respect_the_matchers_trust_floor() {
    use covenant_compute_coordinator::{
        AuditReputationSource, RedundancyConfig, RedundancySampler, SubsidyPolicy,
    };

    let identity = LocalIdentity::generate("coordinator@e2e");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    let config = CoordinatorConfig {
        long_poll_timeout: Duration::from_secs(2),
        subsidy_policy: Some(SubsidyPolicy::new(10_000, 1_000_000).unwrap()),
        // Below-neutral operators are out: 5_000 is the unknown prior,
        // so only a proven failure record lands under this floor.
        min_operator_score_bps: 4_000,
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::new(identity, config, reputation, payout, audit);
    let base_url = spawn_coordinator(state.clone()).await;

    let source_identity = LocalIdentity::generate("source@e2e");
    let register_req = RegisterRequest::sign(
        cpu_profile(&source_identity, 800),
        payout_addr(24),
        &source_identity,
    )
    .unwrap();
    state
        .registry()
        .register(&register_req, epoch_ms(), None, false)
        .unwrap();
    // The floored operator asks CHEAPER than the clean one — price
    // must not buy its way past the floor.
    let mut mirrors = Vec::new();
    for (display, ask) in [("bad-mirror@e2e", 900u64), ("good-mirror@e2e", 1_000)] {
        let identity = LocalIdentity::generate(display);
        let register_req =
            RegisterRequest::sign(cpu_profile(&identity, ask), payout_for(display), &identity)
                .unwrap();
        state
            .registry()
            .register(&register_req, epoch_ms(), None, false)
            .unwrap();
        mirrors.push(identity.agent_id().pubkey_base58());
    }
    let (bad_mirror, good_mirror) = (mirrors[0].clone(), mirrors[1].clone());
    // Four attributed faults: (0+1)/(0+4+2) = 1_666, under the floor.
    for _ in 0..4 {
        state
            .record_audit(AuditKind::ComputeJobRefunded {
                job_id: Uuid::new_v4(),
                reason: "deadline_expired".into(),
                operator_pubkey_b58: Some(bad_mirror.clone()),
            })
            .await;
    }

    let buyer = LocalIdentity::generate("buyer@e2e");
    complete_one_job(
        &base_url,
        &source_identity,
        &buyer,
        Uuid::new_v4(),
        30_000,
        "floored",
    )
    .await;

    let sampler = RedundancySampler::new(state.clone(), RedundancyConfig::default());
    let report = sampler.tick().await;
    let (_, dispatched) = report.dispatched.expect("the clean mirror still samples");
    assert_eq!(
        dispatched.len(),
        1,
        "only the clean operator drew a mirror: {dispatched:?}"
    );
    assert_eq!(dispatched[0].1, good_mirror);
    assert!(
        !dispatched.iter().any(|(_, op)| *op == bad_mirror),
        "a proven-bad operator is not a measuring instrument, however cheap"
    );

    // The floored operator can SEE why it wins nothing: the public
    // reputation read carries the floor, the score under it, and the
    // matchable verdict — the answer `covenant-compute-node status`
    // renders for an operator asking "why am I not earning".
    let http = reqwest::Client::new();
    let standing = |op: String| {
        let http = http.clone();
        let base_url = base_url.clone();
        async move {
            http.get(format!("{base_url}/federation/operators/{op}/reputation"))
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap()
        }
    };
    let floored = standing(bad_mirror).await;
    assert_eq!(floored["registered"], true);
    assert_eq!(
        floored["live"], true,
        "floored, not gone — the distinction the view exists for"
    );
    assert_eq!(floored["matchable"], false);
    assert_eq!(floored["min_score_bps"], 4_000);
    assert!(floored["score_bps"].as_u64().unwrap() < 4_000);
    let clean = standing(good_mirror).await;
    assert_eq!(clean["matchable"], true);
}

/// The anti-faucet inheritance, mirror edition: sampling spend is
/// bootstrap-tagged, so a coordinator with the subsidy kill-switch
/// closed dispatches no mirrors at all — and writes no marker rows.
#[tokio::test]
async fn redundancy_mirrors_stop_dead_without_a_subsidy_policy() {
    use covenant_compute_coordinator::{RedundancyConfig, RedundancySampler};

    let (state, _payout) = new_coordinator_state(Duration::from_secs(2));
    let base_url = spawn_coordinator(state.clone()).await;

    let source_identity = LocalIdentity::generate("source@e2e");
    let register_req = RegisterRequest::sign(
        cpu_profile(&source_identity, 800),
        payout_addr(24),
        &source_identity,
    )
    .unwrap();
    state
        .registry()
        .register(&register_req, epoch_ms(), None, false)
        .unwrap();
    let mirror_identity = LocalIdentity::generate("mirror@e2e");
    let register_req = RegisterRequest::sign(
        cpu_profile(&mirror_identity, 900),
        payout_addr(13),
        &mirror_identity,
    )
    .unwrap();
    state
        .registry()
        .register(&register_req, epoch_ms(), None, false)
        .unwrap();

    // One released batch job on the books (receipt fabricated through
    // the ordinary result endpoint — no node needs to poll for it).
    let buyer = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    complete_one_job(
        &base_url,
        &source_identity,
        &buyer,
        job_id,
        30_000,
        "no-subsidy",
    )
    .await;

    let sampler = RedundancySampler::new(state.clone(), RedundancyConfig::default());
    let report = sampler.tick().await;
    assert!(report.dispatched.is_none());
    let reason = report.idle_reason.expect("an idle reason is reported");
    assert!(reason.contains("hold refused"), "got: {reason}");
    let events = state.audit().recent(usize::MAX).await.unwrap();
    assert!(
        events
            .iter()
            .all(|e| !matches!(e.kind, AuditKind::ComputeRedundancyDispatched { .. })),
        "a refused hold leaves no dispatch marker"
    );
}

/// Completes a job as a named operator with a chosen output, so a test
/// can make operators agree or diverge on the receipt hash.
async fn submit_output(
    base_url: &str,
    job_id: Uuid,
    operator: &LocalIdentity,
    output: Vec<Content>,
) {
    let http = reqwest::Client::new();
    let receipt = SignedWorkReceipt::sign(
        WorkReceiptPayload {
            job_id,
            operator: operator.agent_id(),
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
            status: A2ATaskStatus::Ok,
            executed_at_ms: epoch_ms(),
            node_audit_root_hex: "cc".repeat(32),
        },
        operator,
    )
    .unwrap();
    let resp = http
        .post(format!("{base_url}/federation/jobs/{job_id}/result"))
        .json(&covenant_compute_protocol::JobResultMessage { receipt, output })
        .send()
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );
}

/// The inference half of C5, end to end: with the inference toggle on, a
/// temperature-0 seeded job is re-bought from other operators, and the
/// operator whose output diverges from the honest majority is faulted —
/// the exact gap that let a signature-valid receipt over junk inference
/// get paid.
#[tokio::test]
async fn a_divergent_deterministic_inference_operator_is_faulted() {
    use covenant_compute_coordinator::{
        AuditReputationSource, RedundancyConfig, RedundancySampler, SubsidyPolicy,
    };
    use std::collections::HashMap;

    let identity = LocalIdentity::generate("coordinator@e2e");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    let config = CoordinatorConfig {
        long_poll_timeout: Duration::from_secs(2),
        subsidy_policy: Some(SubsidyPolicy::new(10_000, 1_000_000).unwrap()),
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::new(identity, config, reputation, payout, audit);
    let base_url = spawn_coordinator(state.clone()).await;
    let http = reqwest::Client::new();

    // Only the source operator is live when the buyer's job lands, so it
    // is the one matched — no need to guess the matcher's pick. It serves
    // JUNK for a job the buyer pinned to greedy, seeded decoding.
    let source = LocalIdentity::generate("source@e2e");
    let mut source_profile = cpu_profile(&source, 1_000);
    source_profile.job_kinds = vec![JobKind::InferenceCall];
    state
        .registry()
        .register(
            &RegisterRequest::sign(source_profile, payout_for("source@e2e"), &source).unwrap(),
            epoch_ms(),
            None,
            false,
        )
        .unwrap();

    let buyer = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let mut payload =
        signed_envelope(&buyer, job_id, 1_000, 30_000, epoch_ms(), "det-infer").payload;
    payload.kind = JobKind::InferenceCall;
    payload.capability_requirement.kind = JobKind::InferenceCall;
    payload.input = vec![
        Content::text("square(n)?"),
        Content::json(serde_json::json!({ "generation": { "temperature": 0.0, "seed": 7 } })),
    ];
    let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
    assert_eq!(
        http.post(format!("{base_url}/federation/jobs"))
            .json(&envelope)
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::ACCEPTED
    );
    submit_output(&base_url, job_id, &source, vec![Content::text("JUNK")]).await;

    // Now bring up two honest mirror operators and turn inference
    // sampling on.
    let mut mirrors: HashMap<String, LocalIdentity> = HashMap::new();
    for (display, ask) in [("mirror-a@e2e", 1_100u64), ("mirror-b@e2e", 1_200)] {
        let op = LocalIdentity::generate(display);
        let mut profile = cpu_profile(&op, ask);
        profile.job_kinds = vec![JobKind::InferenceCall];
        state
            .registry()
            .register(
                &RegisterRequest::sign(profile, payout_for(display), &op).unwrap(),
                epoch_ms(),
                None,
                false,
            )
            .unwrap();
        mirrors.insert(op.agent_id().pubkey_base58(), op);
    }
    let sampler = RedundancySampler::new(
        state.clone(),
        RedundancyConfig {
            sample_inference: true,
            ..RedundancyConfig::default()
        },
    );

    let report = sampler.tick().await;
    let (sampled, dispatched) = report
        .dispatched
        .expect("a deterministic inference job is sampled");
    assert_eq!(sampled, job_id);
    assert_eq!(
        dispatched.len(),
        2,
        "both mirrors drew a job: {dispatched:?}"
    );

    // The mirrors do the real work and agree on the honest answer.
    for (mirror_job_id, op) in &dispatched {
        submit_output(
            &base_url,
            *mirror_job_id,
            &mirrors[op],
            vec![Content::text("n * n")],
        )
        .await;
    }

    // Judge: the source is the strict-majority minority — faulted.
    let source_key = source.agent_id().pubkey_base58();
    let report = sampler.tick().await;
    let source_verdict = report
        .judged
        .iter()
        .find(|v| v.operator_pubkey_b58 == source_key)
        .expect("the source operator was judged");
    assert_eq!(
        source_verdict.agreed,
        Some(false),
        "junk output against an honest majority is a fault: {source_verdict:?}"
    );
    for (_, op) in &dispatched {
        let v = report
            .judged
            .iter()
            .find(|v| &v.operator_pubkey_b58 == op)
            .expect("each honest mirror is judged");
        assert_eq!(v.agreed, Some(true), "the honest mirrors agreed");
    }

    // Reputation counts it: the source now carries a redundancy fault
    // row, next to the refund, canary and dispute ones.
    let events = state.audit().recent(usize::MAX).await.unwrap();
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            AuditKind::ComputeRedundancyResult { operator_pubkey_b58, agreed: Some(false), .. }
                if *operator_pubkey_b58 == source_key
        )),
        "the fault is durable on the audit chain"
    );
}

/// C8's outflow side: recording an out-of-band partner payout is
/// admin-token gated (fail closed), idempotent by reference, refuses
/// to outrun the accrual books, and shows up in the public partners
/// view as paid/outstanding.
#[tokio::test]
async fn partner_payout_records_are_gated_idempotent_and_bounded_by_accruals() {
    use covenant_compute_coordinator::{JobPhase, JobRecord, PartnerConfig};

    let identity = LocalIdentity::generate("coordinator@e2e");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    let mut partners = std::collections::HashMap::new();
    partners.insert(
        "partner-a".to_string(),
        PartnerConfig::new("partner-a-address".into(), 5_000).unwrap(),
    );
    let config = CoordinatorConfig {
        long_poll_timeout: Duration::from_secs(2),
        partners,
        admin_token: Some("s5-admin-token".into()),
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::new(identity, config, reputation, payout, audit);

    // Two completed jobs with 400 + 350 accrued to partner-a — the
    // accrual pipeline itself is covered by the fee/rev-share tests;
    // here the records are seeded directly.
    for share in [400u64, 350] {
        let buyer = LocalIdentity::generate("buyer@e2e");
        let job_id = Uuid::new_v4();
        let envelope = signed_envelope(&buyer, job_id, 10_000, 30_000, epoch_ms(), "payout-e2e");
        let escrow_hold = covenant_compute_protocol::EscrowHoldAttestation::sign(
            job_id,
            10_000,
            FundingSource::Organic,
            epoch_ms(),
            &LocalIdentity::generate("coordinator@e2e"),
        )
        .unwrap();
        state
            .jobs()
            .insert(
                job_id,
                JobRecord {
                    operator_pubkey_b58: "op".into(),
                    payout_address: "op-addr".into(),
                    envelope,
                    escrow_hold,
                    phase: JobPhase::Completed,
                    receipt: None,
                    output: None,
                    fee_micro_usdc: share * 2,
                    referral_code: Some("partner-a".into()),
                    partner_share_micro_usdc: share,
                    buyer_referral_code: None,
                    buyer_partner_share_micro_usdc: 0,
                    payout: None,
                    concluded_at_ms: None,
                    refund_reason: None,
                    dispute: None,
                    offered_at_ms: 0,
                    pinned: false,
                    accepted_at_ms: None,
                    metered_elapsed_ms: None,
                    close_requested_at_ms: None,
                    lease_access: None,
                    check_jobs: Vec::new(),
                    checks_task: None,
                    hidden_checks: None,
                    vote_round: None,
                    rework: None,
                    order: None,
                },
            )
            .unwrap();
    }
    let base_url = spawn_coordinator(state.clone()).await;
    let http = reqwest::Client::new();
    let payouts_url = format!("{base_url}/federation/partners/partner-a/payouts");

    // No token / wrong token: 401 before anything is read.
    let unsigned = http
        .post(&payouts_url)
        .json(&serde_json::json!({"amount_micro_usdc": 100, "reference": "tx-1"}))
        .send()
        .await
        .unwrap();
    assert_eq!(unsigned.status(), 401);
    let wrong = http
        .post(&payouts_url)
        .bearer_auth("not-the-token")
        .json(&serde_json::json!({"amount_micro_usdc": 100, "reference": "tx-1"}))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);

    // Zero moves nothing and a blank reference records nothing
    // traceable; both refuse before the books are touched.
    let zero = http
        .post(&payouts_url)
        .bearer_auth("s5-admin-token")
        .json(&serde_json::json!({"amount_micro_usdc": 0, "reference": "tx-zero"}))
        .send()
        .await
        .unwrap();
    assert_eq!(zero.status(), 400);
    assert!(zero
        .text()
        .await
        .unwrap()
        .contains("amount must be positive"));
    let blank = http
        .post(&payouts_url)
        .bearer_auth("s5-admin-token")
        .json(&serde_json::json!({"amount_micro_usdc": 100, "reference": "   "}))
        .send()
        .await
        .unwrap();
    assert_eq!(blank.status(), 400);
    assert!(blank
        .text()
        .await
        .unwrap()
        .contains("reference is required"));

    // Recording 500 of the 750 outstanding.
    let recorded: serde_json::Value = http
        .post(&payouts_url)
        .bearer_auth("s5-admin-token")
        .json(&serde_json::json!({"amount_micro_usdc": 500, "reference": "tx-1"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(recorded["recorded"], true);
    assert_eq!(recorded["paid_micro_usdc"], 500);
    assert_eq!(recorded["outstanding_micro_usdc"], 250);

    // The same reference again is an honest retry: nothing moves.
    let retried: serde_json::Value = http
        .post(&payouts_url)
        .bearer_auth("s5-admin-token")
        .json(&serde_json::json!({"amount_micro_usdc": 500, "reference": "tx-1"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(retried["recorded"], false);
    assert_eq!(retried["paid_micro_usdc"], 500);

    // Paying more than the books ever accrued is refused.
    let over = http
        .post(&payouts_url)
        .bearer_auth("s5-admin-token")
        .json(&serde_json::json!({"amount_micro_usdc": 251, "reference": "tx-2"}))
        .send()
        .await
        .unwrap();
    assert_eq!(over.status(), 409);

    // A code with no accruals is a 404, not an open ledger.
    let unknown = http
        .post(format!("{base_url}/federation/partners/nobody/payouts"))
        .bearer_auth("s5-admin-token")
        .json(&serde_json::json!({"amount_micro_usdc": 1, "reference": "tx-3"}))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), 404);

    // The public books now show paid and outstanding — amounts only.
    // The payout address is a business relationship, not a
    // transparency figure: absent publicly, present under the admin
    // bearer, and a WRONG bearer is refused rather than quietly
    // redacted (C9).
    let views: serde_json::Value = http
        .get(format!("{base_url}/federation/partners"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = views
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["referral_code"] == "partner-a")
        .expect("partner-a row");
    assert_eq!(row["accrued_micro_usdc"], 750);
    assert_eq!(row["paid_micro_usdc"], 500);
    assert_eq!(row["outstanding_micro_usdc"], 250);
    assert!(
        row.get("payout_address").is_none(),
        "public partner books must not name payout addresses"
    );
    let wrong_bearer = http
        .get(format!("{base_url}/federation/partners"))
        .bearer_auth("not-the-token")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_bearer.status(), 401);
    let admin_views: serde_json::Value = http
        .get(format!("{base_url}/federation/partners"))
        .bearer_auth("s5-admin-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let admin_row = admin_views
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["referral_code"] == "partner-a")
        .expect("partner-a admin row");
    assert_eq!(admin_row["payout_address"], "partner-a-address");

    // And the audit chain carries exactly one paid row (the retry
    // recorded nothing).
    let events = state.audit().recent(50).await.unwrap();
    let paid_rows: Vec<_> = events
        .iter()
        .filter(|e| {
            matches!(
                &e.kind,
                AuditKind::ComputePartnerSharePaid { referral_code, amount_micro_usdc: 500, .. }
                    if referral_code == "partner-a"
            )
        })
        .collect();
    assert_eq!(paid_rows.len(), 1);
}

/// The admin surface fails closed: with no admin token configured —
/// the default — presenting a bearer is refused with "disabled", not
/// matched against anything, on every admin route.
#[tokio::test]
async fn the_admin_surface_fails_closed_when_no_token_is_configured() {
    let (state, _payout) = new_coordinator_state(Duration::from_secs(2));
    let base_url = spawn_coordinator(state).await;
    let http = reqwest::Client::new();

    let payout = http
        .post(format!("{base_url}/federation/partners/any-code/payouts"))
        .bearer_auth("any-token")
        .json(&serde_json::json!({"amount_micro_usdc": 1, "reference": "tx"}))
        .send()
        .await
        .unwrap();
    assert_eq!(payout.status(), 401);
    assert!(payout
        .text()
        .await
        .unwrap()
        .contains("admin surface is disabled"));

    // The partner listing is public without a bearer but must not
    // treat an unverifiable bearer as the admin view.
    let partners = http
        .get(format!("{base_url}/federation/partners"))
        .bearer_auth("any-token")
        .send()
        .await
        .unwrap();
    assert_eq!(partners.status(), 401);

    // The subsidy close is the same fail-closed surface: no configured
    // token, no runtime kill — the boot-only posture stays intact.
    let close = http
        .post(format!("{base_url}/federation/subsidy/close"))
        .bearer_auth("any-token")
        .send()
        .await
        .unwrap();
    assert_eq!(close.status(), 401);
}

/// The C6 kill-switch closes at runtime and STAYS closed — end to end.
/// Admin-gated both ways (no bearer, wrong bearer), one real close
/// however often it's retried (exactly one audit row, spend-at-close
/// pinned inside it), immediate on the wire (the identical envelope
/// shape that was 202 before the close is 409 after), visible to
/// scrapers (`compute_subsidy_closed`), and — the reason the close
/// journals before it latches — a restart whose config still carries
/// the subsidy policy boots with the switch shut instead of silently
/// re-arming from the environment.
#[tokio::test]
async fn the_subsidy_close_is_admin_gated_one_way_and_survives_a_rearming_restart() {
    let dir = tempfile::tempdir().unwrap();
    let identity_path = dir.path().join("identity.json");
    let journal_path = dir.path().join("journal.jsonl");

    let durable_state = || {
        let identity =
            LocalIdentity::load_or_create(&identity_path, "coordinator@subsidy-close").unwrap();
        let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
        let journal_path = journal_path.clone();
        async move {
            CoordinatorState::with_journal(
                identity,
                CoordinatorConfig {
                    long_poll_timeout: Duration::from_millis(200),
                    default_funding_source: FundingSource::Bootstrap,
                    subsidy_policy: Some(
                        covenant_compute_coordinator::SubsidyPolicy::new(10_000, 1_000_000)
                            .unwrap(),
                    ),
                    admin_token: Some("the-operator-secret".into()),
                    ..CoordinatorConfig::default()
                },
                Arc::new(AuditReputationSource::new(audit.clone())),
                Arc::new(MockPayout::new()),
                audit,
                &journal_path,
                None,
            )
            .await
            .unwrap()
        }
    };

    // Life 1: armed. A registered operator makes bootstrap admission
    // observable as a 202.
    let state1 = durable_state().await;
    let (url1, server1) = spawn_coordinator_abortable(state1.clone()).await;
    let http = reqwest::Client::new();

    let operator_identity = LocalIdentity::generate("operator@subsidy-close");
    let client1 = HttpCoordinatorClient::with_config(url1.clone(), Duration::from_secs(2), 1);
    client1
        .register(
            RegisterRequest::sign(
                cpu_profile(&operator_identity, 1_000),
                payout_addr(77),
                &operator_identity,
            )
            .unwrap(),
        )
        .await
        .unwrap();

    let view: serde_json::Value = http
        .get(format!("{url1}/federation/subsidy"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(view["enforced"], true);
    assert_eq!(view["closed"], false);

    let buyer = LocalIdentity::generate("buyer@subsidy-close");
    let admitted = signed_envelope(
        &buyer,
        Uuid::new_v4(),
        1_000,
        30_000,
        epoch_ms(),
        "subsidy-close-before",
    );
    let resp = http
        .post(format!("{url1}/federation/jobs"))
        .json(&admitted)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);

    // The gate: no bearer and a wrong bearer both bounce off the
    // configured token, and neither touches the latch.
    let bare = http
        .post(format!("{url1}/federation/subsidy/close"))
        .send()
        .await
        .unwrap();
    assert_eq!(bare.status(), 401);
    let forged = http
        .post(format!("{url1}/federation/subsidy/close"))
        .bearer_auth("not-the-secret")
        .send()
        .await
        .unwrap();
    assert_eq!(forged.status(), 401);
    assert!(!state1.escrow().subsidy_status().closed);

    // The real close answers the shut books; an honest retry echoes
    // them without closing anything twice.
    for _ in 0..2 {
        let closed: serde_json::Value = http
            .post(format!("{url1}/federation/subsidy/close"))
            .bearer_auth("the-operator-secret")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(closed["enforced"], false);
        assert_eq!(closed["closed"], true);
        assert_eq!(closed["remaining_micro_usdc"], 0);
    }
    let events = state1.audit().recent(50).await.unwrap();
    let close_rows: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.kind {
            AuditKind::ComputeSubsidyClosed {
                bootstrap_committed_micro_usdc,
            } => Some(*bootstrap_committed_micro_usdc),
            _ => None,
        })
        .collect();
    assert_eq!(close_rows, vec![1_000], "one close, spend-at-close pinned");

    let metrics = http
        .get(format!("{url1}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(metrics.contains("compute_subsidy_closed 1"), "{metrics}");

    // The exact envelope shape that was money moments ago is refused,
    // named as the subsidy's doing.
    let refused = signed_envelope(
        &buyer,
        Uuid::new_v4(),
        1_000,
        30_000,
        epoch_ms(),
        "subsidy-close-after",
    );
    let resp = http
        .post(format!("{url1}/federation/jobs"))
        .json(&refused)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    assert!(resp
        .text()
        .await
        .unwrap()
        .contains("bootstrap subsidy exhausted"));

    server1.abort();

    // Life 2: the config above re-supplies the policy — the exact
    // silent-re-arm shape. The journaled close outranks it.
    let state2 = durable_state().await;
    let (url2, _server2) = spawn_coordinator_abortable(state2.clone()).await;
    let view: serde_json::Value = http
        .get(format!("{url2}/federation/subsidy"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(view["enforced"], false);
    assert_eq!(view["closed"], true);

    let rearmed = signed_envelope(
        &buyer,
        Uuid::new_v4(),
        1_000,
        30_000,
        epoch_ms(),
        "subsidy-close-rearm",
    );
    let resp = http
        .post(format!("{url2}/federation/jobs"))
        .json(&rearmed)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    assert!(resp
        .text()
        .await
        .unwrap()
        .contains("bootstrap subsidy exhausted"));
}

/// Drives one job through submit -> result against a live coordinator,
/// returning the buyer identity. The operator's receipt is honest (the
/// output hashes, the status is Ok) — what the dispute tests then argue
/// about is the CONTENT, which is exactly the fault class disputes
/// exist for.
async fn complete_one_job(
    base_url: &str,
    operator_identity: &LocalIdentity,
    buyer_identity: &LocalIdentity,
    job_id: Uuid,
    deadline_ms: u64,
    idem_key: &str,
) {
    let http = reqwest::Client::new();
    let envelope = signed_envelope(
        buyer_identity,
        job_id,
        1_000,
        deadline_ms,
        epoch_ms(),
        idem_key,
    );
    let submit = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit.status(), reqwest::StatusCode::ACCEPTED);

    let output = vec![Content::text("forty-two")];
    let receipt = SignedWorkReceipt::sign(
        WorkReceiptPayload {
            job_id,
            operator: operator_identity.agent_id(),
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
            status: A2ATaskStatus::Ok,
            executed_at_ms: epoch_ms(),
            node_audit_root_hex: "cc".repeat(32),
        },
        operator_identity,
    )
    .unwrap();
    let result = http
        .post(format!("{base_url}/federation/jobs/{job_id}/result"))
        .json(&covenant_compute_protocol::JobResultMessage { receipt, output })
        .send()
        .await
        .unwrap();
    assert_eq!(result.status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn a_buyer_dispute_lands_once_and_faults_reputation() {
    let (state, _payout) = new_coordinator_state(Duration::from_secs(2));
    let state_handle = state.clone();
    let base_url = spawn_coordinator(state).await;
    let http = reqwest::Client::new();

    let operator_identity = LocalIdentity::generate("operator@e2e");
    let operator_key = operator_identity.agent_id().pubkey_base58();
    let register_req = RegisterRequest::sign(
        cpu_profile(&operator_identity, 1_000),
        payout_addr(1),
        &operator_identity,
    )
    .unwrap();
    HttpCoordinatorClient::with_config(base_url.clone(), Duration::from_secs(2), 1)
        .register(register_req)
        .await
        .unwrap();

    let buyer_identity = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    complete_one_job(
        &base_url,
        &operator_identity,
        &buyer_identity,
        job_id,
        30_000,
        "e2e-dispute",
    )
    .await;

    let dispute_url = format!("{base_url}/federation/jobs/{job_id}/dispute");

    // A stranger's signed dispute of someone else's job: 401.
    let stranger = LocalIdentity::generate("stranger@e2e");
    let foreign = covenant_compute_protocol::DisputeRequest::sign(
        stranger.agent_id(),
        job_id,
        "not even my job".into(),
        epoch_ms(),
        &stranger,
    )
    .unwrap();
    let resp = http.post(&dispute_url).json(&foreign).send().await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    // A stale signature: 401 even from the right buyer.
    let stale = covenant_compute_protocol::DisputeRequest::sign(
        buyer_identity.agent_id(),
        job_id,
        "captured yesterday".into(),
        epoch_ms() - covenant_compute_protocol::DISPUTE_MAX_SKEW_MS - 1_000,
        &buyer_identity,
    )
    .unwrap();
    let resp = http.post(&dispute_url).json(&stale).send().await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    // A dispute signed for this job but aimed at another job's path is
    // refused before it verifies — the path and the signed job_id have
    // to name the same job, so a complaint can't be misrouted onto a
    // job it never named.
    let misrouted = covenant_compute_protocol::DisputeRequest::sign(
        buyer_identity.agent_id(),
        job_id,
        "meant for another job".into(),
        epoch_ms(),
        &buyer_identity,
    )
    .unwrap();
    let resp = http
        .post(format!(
            "{base_url}/federation/jobs/{}/dispute",
            Uuid::new_v4()
        ))
        .json(&misrouted)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // A dispute reworded after signing no longer verifies: the reason
    // rides the journal as signed evidence, so a relay can't put words
    // in the buyer's mouth.
    let mut reworded = covenant_compute_protocol::DisputeRequest::sign(
        buyer_identity.agent_id(),
        job_id,
        "the operator was honest".into(),
        epoch_ms(),
        &buyer_identity,
    )
    .unwrap();
    reworded.reason = "the operator returned garbage".into();
    let resp = http
        .post(&dispute_url)
        .json(&reworded)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body = resp.text().await.unwrap();
    assert!(body.contains("dispute does not verify"), "got: {body}");

    // The buyer's real dispute: recorded.
    let dispute = covenant_compute_protocol::DisputeRequest::sign(
        buyer_identity.agent_id(),
        job_id,
        "output was unrelated to the prompt".into(),
        epoch_ms(),
        &buyer_identity,
    )
    .unwrap();
    let resp = http.post(&dispute_url).json(&dispute).send().await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let view: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(view["disputed"], true);
    assert_eq!(view["operator_pubkey_b58"], operator_key);

    // Exactly once: an identical retry (or a rival second complaint)
    // conflicts.
    let resp = http.post(&dispute_url).json(&dispute).send().await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);

    // The dispute survives on the record and in the books both sides
    // read; the money stays released.
    let record = state_handle.jobs().get(job_id).unwrap();
    assert_eq!(
        record.phase,
        covenant_compute_coordinator::JobPhase::Completed
    );
    assert_eq!(
        record.dispute.as_ref().map(|d| d.reason.as_str()),
        Some("output was unrelated to the prompt")
    );
    record
        .dispute
        .unwrap()
        .verify()
        .expect("kept verbatim, still verifies");
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released,
        "a dispute never claws back a receipt-verified release"
    );

    // Reputation: the release stands, the dispute is a fault with its
    // own counter — read over HTTP like any buyer deciding whom to
    // trust.
    let reputation: serde_json::Value = http
        .get(format!(
            "{base_url}/federation/operators/{operator_key}/reputation"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reputation["released"], 1);
    assert_eq!(reputation["faults"], 1);
    assert_eq!(reputation["disputed"], 1);
    assert_eq!(reputation["score_bps"], 5_000);

    // The operator's own signed books read names the disputed job and
    // carries the buyer's complaint verbatim — the reputation counter
    // above, made addressable.
    let books_path = format!("/federation/operators/{operator_key}/jobs");
    let books_signed_at = epoch_ms();
    let books_sig =
        covenant_compute_protocol::sign_read(&operator_identity, &books_path, books_signed_at)
            .unwrap();
    let books: serde_json::Value = http
        .get(format!("{base_url}{books_path}"))
        .header(
            covenant_compute_protocol::READ_SIGNED_AT_HEADER,
            books_signed_at.to_string(),
        )
        .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, books_sig)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = &books.as_array().expect("a row array")[0];
    assert_eq!(row["job_id"], job_id.to_string());
    assert_eq!(row["disputed"], true);
    assert_eq!(
        row["dispute_reason"], "output was unrelated to the prompt",
        "the signed complaint rides the operator's own books"
    );

    let events = state_handle.audit().recent(20).await.unwrap();
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        AuditKind::ComputeJobDisputed { job_id: id, operator_pubkey_b58, .. }
            if *id == job_id && *operator_pubkey_b58 == operator_key
    )));
}

#[tokio::test]
async fn disputes_bounce_on_unconcluded_refunded_and_window_closed_jobs() {
    // Window of zero: every dispute of a concluded job is already too
    // late — the sharpest way to pin the window check without waiting.
    let identity = LocalIdentity::generate("coordinator@e2e");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let config = CoordinatorConfig {
        long_poll_timeout: Duration::from_secs(2),
        dispute_window: Duration::ZERO,
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::new(
        identity,
        config,
        reputation,
        Arc::new(MockPayout::new()),
        audit,
    );
    let state_handle = state.clone();
    let base_url = spawn_coordinator(state).await;
    let http = reqwest::Client::new();

    let operator_identity = LocalIdentity::generate("operator@e2e");
    let register_req = RegisterRequest::sign(
        cpu_profile(&operator_identity, 1_000),
        payout_addr(1),
        &operator_identity,
    )
    .unwrap();
    HttpCoordinatorClient::with_config(base_url.clone(), Duration::from_secs(2), 1)
        .register(register_req)
        .await
        .unwrap();
    let buyer_identity = LocalIdentity::generate("buyer@e2e");

    // In flight (offered, no result yet): nothing concluded to dispute.
    let in_flight = Uuid::new_v4();
    let envelope = signed_envelope(
        &buyer_identity,
        in_flight,
        1_000,
        30_000,
        epoch_ms(),
        "e2e-dispute-inflight",
    );
    let submit = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit.status(), reqwest::StatusCode::ACCEPTED);
    let dispute = |job_id: Uuid, reason: &str| {
        covenant_compute_protocol::DisputeRequest::sign(
            buyer_identity.agent_id(),
            job_id,
            reason.into(),
            epoch_ms(),
            &buyer_identity,
        )
        .unwrap()
    };
    let resp = http
        .post(format!("{base_url}/federation/jobs/{in_flight}/dispute"))
        .json(&dispute(in_flight, "too slow"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);

    // Refunded (operator rejected -> money already back): nothing owed.
    state_handle
        .jobs()
        .set_phase(in_flight, covenant_compute_coordinator::JobPhase::Rejected)
        .unwrap();
    let resp = http
        .post(format!("{base_url}/federation/jobs/{in_flight}/dispute"))
        .json(&dispute(in_flight, "rejected me"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);

    // Completed, but the (zero) window has passed: too late.
    let concluded = Uuid::new_v4();
    complete_one_job(
        &base_url,
        &operator_identity,
        &buyer_identity,
        concluded,
        30_000,
        "e2e-dispute-window",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let resp = http
        .post(format!("{base_url}/federation/jobs/{concluded}/dispute"))
        .json(&dispute(concluded, "window practice"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("window"),
        "got: {body}"
    );

    // A completed job whose signed issued_at_ms + deadline_ms overflows
    // u64 must answer the dispute like any other, not panic: the
    // concluded-time fallback saturates the way admission and the sweep
    // already do. Before that fix the eager `unwrap_or` default computed
    // the sum unconditionally and tripped the release build's
    // overflow-checks, resetting the connection.
    let overflowing = Uuid::new_v4();
    complete_one_job(
        &base_url,
        &operator_identity,
        &buyer_identity,
        overflowing,
        u64::MAX,
        "e2e-dispute-overflow",
    )
    .await;
    let resp = http
        .post(format!("{base_url}/federation/jobs/{overflowing}/dispute"))
        .json(&dispute(overflowing, "overflowing deadline"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);

    // An unknown job stays a plain 404.
    let unknown = Uuid::new_v4();
    let resp = http
        .post(format!("{base_url}/federation/jobs/{unknown}/dispute"))
        .json(&dispute(unknown, "never existed"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

/// The buyer's escape hatch, end to end: a job stuck `Offered` behind
/// an assignee that never polls wedges the buyer's in-flight ceiling
/// until its deadline — unless the buyer cancels. The cancel refunds
/// the hold now, frees the ceiling for the next submission, pulls the
/// dead offer out of the assignee's queue, faults nobody, and answers
/// an honest retry with the same fact instead of an error.
#[tokio::test]
async fn a_buyer_cancels_an_unaccepted_job_and_the_hold_comes_straight_back() {
    let identity = LocalIdentity::generate("coordinator@e2e");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let config = CoordinatorConfig {
        long_poll_timeout: Duration::from_millis(300),
        max_inflight_per_buyer: Some(1),
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::new(
        identity,
        config,
        reputation,
        Arc::new(MockPayout::new()),
        audit,
    );
    let base_url = spawn_coordinator(state.clone()).await;
    let http = reqwest::Client::new();

    // The assignee registers and then never polls — the offer will sit
    // until the deadline sweep unless the buyer acts.
    let idle_operator = LocalIdentity::generate("idle-operator@e2e");
    let idle_key = idle_operator.agent_id().pubkey_base58();
    let resp = http
        .post(format!("{base_url}/federation/operators/register"))
        .json(
            &RegisterRequest::sign(
                cpu_profile(&idle_operator, 500),
                payout_addr(8),
                &idle_operator,
            )
            .unwrap(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let buyer = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let envelope = signed_envelope(&buyer, job_id, 1_000, 120_000, epoch_ms(), "e2e-cancel");
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);

    // The buyer is wedged: the ceiling counts the stuck offer.
    let wedged = Uuid::new_v4();
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&signed_envelope(
            &buyer,
            wedged,
            1_000,
            120_000,
            epoch_ms(),
            "e2e-cancel-wedged",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);

    // The cancel: money back in full, phase concluded, queue cleaned.
    let cancel_url = format!("{base_url}/federation/jobs/{job_id}/cancel");
    let cancel = covenant_compute_protocol::CancelRequest::sign(
        buyer.agent_id(),
        job_id,
        epoch_ms(),
        &buyer,
    )
    .unwrap();
    let resp = http.post(&cancel_url).json(&cancel).send().await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let view: covenant_compute_protocol::CancelView = resp.json().await.unwrap();
    assert_eq!(view.job_id, job_id);
    assert_eq!(view.status, "refunded");
    assert_eq!(view.refunded_micro_usdc, 1_000);
    assert_eq!(
        state.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Refunded
    );
    let record = state.jobs().get(job_id).unwrap();
    assert_eq!(
        record.phase,
        covenant_compute_coordinator::JobPhase::Refunded
    );
    assert_eq!(
        record.refund_reason,
        Some(covenant_compute_protocol::RefundReason::BuyerCancelled)
    );
    assert!(
        !state.registry().queue_holds(&idle_key, job_id),
        "the withdrawn offer leaves the assignee's delivery queue"
    );

    // An honest retry of the same cancellation answers the fact, not
    // an error.
    let resp = http.post(&cancel_url).json(&cancel).send().await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let view: covenant_compute_protocol::CancelView = resp.json().await.unwrap();
    assert_eq!(view.status, "refunded");

    // The ceiling is free again: the next submission lands.
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&signed_envelope(
            &buyer,
            Uuid::new_v4(),
            1_000,
            120_000,
            epoch_ms(),
            "e2e-cancel-freed",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);

    // Nobody's fault: the refund row is unattributed and the idle
    // operator's standing is untouched.
    let reputation: serde_json::Value = http
        .get(format!(
            "{base_url}/federation/operators/{idle_key}/reputation"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reputation["faults"], 0);
    let events = state.audit().recent(20).await.unwrap();
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        AuditKind::ComputeJobRefunded { job_id: id, reason, operator_pubkey_b58: None }
            if *id == job_id && reason == "buyer_cancelled"
    )));

    // And the operator can read that fact itself: its job books call
    // the row `buyer_cancelled`, so a refunded row in its history is
    // distinguishable from a deadline fault without asking anyone.
    let books_path = format!("/federation/operators/{idle_key}/jobs");
    let books_signed_at = epoch_ms();
    let books_sig =
        covenant_compute_protocol::sign_read(&idle_operator, &books_path, books_signed_at).unwrap();
    let books: serde_json::Value = http
        .get(format!("{base_url}{books_path}"))
        .header(
            covenant_compute_protocol::READ_SIGNED_AT_HEADER,
            books_signed_at.to_string(),
        )
        .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, books_sig)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = books
        .as_array()
        .expect("a row array")
        .iter()
        .find(|r| r["job_id"] == job_id.to_string())
        .expect("the cancelled job is in the operator's books");
    assert_eq!(row["status"], "refunded");
    assert_eq!(row["refund_reason"], "buyer_cancelled");
}

/// A cancel that lands as an assigned operator's result is paying the job
/// must not tell the buyer they were refunded. `AlreadySettled` covers both
/// the deadline sweep refunding a hold and an operator's result RELEASING it
/// (a lost-accept recovery pays a still-`Offered` job without re-accepting),
/// so the cancel path consults the escrow and answers the charge — recording
/// no phantom `buyer_cancelled` refund the buyer never received.
#[tokio::test]
async fn a_cancel_racing_a_paid_delivery_reports_the_charge_not_a_refund() {
    let (state, _payout) = new_coordinator_state(Duration::from_millis(300));
    let base_url = spawn_coordinator(state.clone()).await;
    let http = reqwest::Client::new();

    // An operator so the job matches and stays `Offered` rather than
    // refunding for want of supply.
    let operator = LocalIdentity::generate("operator@e2e");
    let resp = http
        .post(format!("{base_url}/federation/operators/register"))
        .json(
            &RegisterRequest::sign(cpu_profile(&operator, 500), payout_addr(8), &operator).unwrap(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let buyer = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let now_ms = epoch_ms();
    let envelope = signed_envelope(
        &buyer,
        job_id,
        1_000,
        120_000,
        now_ms,
        "e2e-cancel-paid-race",
    );
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);

    // The assignee's result wins the settlement inside the cancel window: it
    // releases the still-`Offered` job's hold, leaving escrow `Released`
    // before the phase concludes.
    let output = vec![Content::text("the answer")];
    let receipt = SignedWorkReceipt::sign(
        WorkReceiptPayload {
            job_id,
            operator: operator.agent_id(),
            job_hash_hex: "aa".repeat(32),
            result_hash_hex: covenant_compute_protocol::output_hash_hex(&output),
            meter: JobMeter {
                wall_ms: 1,
                tokens_in: None,
                tokens_out: None,
                gpu_seconds: None,
                finish_reason: None,
            },
            price_micro_usdc: 1_000,
            status: A2ATaskStatus::Ok,
            executed_at_ms: now_ms,
            node_audit_root_hex: "cc".repeat(32),
        },
        &operator,
    )
    .unwrap();
    state.escrow().release(job_id, &receipt).await.unwrap();
    assert_eq!(
        state.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released,
        "the operator's result released the hold"
    );

    // The buyer's cancel lands after the release. It must answer the charge,
    // not a refund.
    let cancel = covenant_compute_protocol::CancelRequest::sign(
        buyer.agent_id(),
        job_id,
        epoch_ms(),
        &buyer,
    )
    .unwrap();
    let resp = http
        .post(format!("{base_url}/federation/jobs/{job_id}/cancel"))
        .json(&cancel)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CONFLICT,
        "a cancel racing a paid delivery is a conflict, not a refund"
    );

    // The hold stays released — the operator keeps its pay — and no phantom
    // `buyer_cancelled` refund is recorded against the charged job.
    assert_eq!(
        state.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released
    );
    let events = state.audit().recent(20).await.unwrap();
    assert!(
        !events.iter().any(|e| matches!(
            &e.kind,
            AuditKind::ComputeJobRefunded { job_id: id, reason, .. }
                if *id == job_id && reason == "buyer_cancelled"
        )),
        "no buyer_cancelled refund is recorded for a job the operator was paid for"
    );
}

/// The mirror of the paid-delivery race: the buyer's cancel settles the
/// hold first, then the assignee delivers a valid, within-deadline
/// result. It must bounce — settle-once has already refunded the buyer —
/// so no payout is minted for a job the buyer cancelled. This pins
/// `submit_result`'s final guard: it treats `AlreadySettled` as a
/// crash-window refill only when the hold actually reads `Released`,
/// never when a cancel left it `Refunded`. Drop that `Released`
/// condition and an operator is paid out of a refunded hold.
#[tokio::test]
async fn a_result_after_a_winning_cancel_settles_no_payout() {
    let (state, payout) = new_coordinator_state(Duration::from_millis(300));
    let base_url = spawn_coordinator(state.clone()).await;
    let http = reqwest::Client::new();

    let operator = LocalIdentity::generate("operator@e2e");
    let resp = http
        .post(format!("{base_url}/federation/operators/register"))
        .json(
            &RegisterRequest::sign(cpu_profile(&operator, 500), payout_addr(8), &operator).unwrap(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let buyer = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let now_ms = epoch_ms();
    let envelope = signed_envelope(
        &buyer,
        job_id,
        1_000,
        120_000,
        now_ms,
        "e2e-cancel-wins-result",
    );
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);

    // The buyer cancels the still-`Offered` job; the hold refunds.
    let cancel = covenant_compute_protocol::CancelRequest::sign(
        buyer.agent_id(),
        job_id,
        epoch_ms(),
        &buyer,
    )
    .unwrap();
    let resp = http
        .post(format!("{base_url}/federation/jobs/{job_id}/cancel"))
        .json(&cancel)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "the cancel refunds the offered hold"
    );
    assert_eq!(
        state.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Refunded,
        "the buyer's cancel refunded the hold"
    );

    // The assignee delivers a valid result, inside the deadline but too
    // late: the hold is already refunded.
    let output = vec![Content::text("the answer")];
    let receipt = SignedWorkReceipt::sign(
        WorkReceiptPayload {
            job_id,
            operator: operator.agent_id(),
            job_hash_hex: "aa".repeat(32),
            result_hash_hex: covenant_compute_protocol::output_hash_hex(&output),
            meter: JobMeter {
                wall_ms: 1,
                tokens_in: None,
                tokens_out: None,
                gpu_seconds: None,
                finish_reason: None,
            },
            price_micro_usdc: 1_000,
            status: A2ATaskStatus::Ok,
            executed_at_ms: now_ms,
            node_audit_root_hex: "cc".repeat(32),
        },
        &operator,
    )
    .unwrap();
    let resp = http
        .post(format!("{base_url}/federation/jobs/{job_id}/result"))
        .json(&covenant_compute_protocol::JobResultMessage { receipt, output })
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CONFLICT,
        "a result for a cancelled job is a conflict, never a second settlement"
    );

    // The hold stays refunded, no on-chain payout is pushed, and no
    // release is booked against the cancelled job.
    assert_eq!(
        state.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Refunded
    );
    assert!(
        payout.records().is_empty(),
        "no payout is pushed for a cancelled job"
    );
    let events = state.audit().recent(50).await.unwrap();
    assert!(
        !events.iter().any(|e| matches!(
            &e.kind,
            AuditKind::ComputeJobReleased { job_id: id, .. } if *id == job_id
        )),
        "no release is recorded for a job the buyer cancelled"
    );
}

/// A cancel is never a clawback: once the assignee has accepted, the
/// work is committed and the buyer's only outs are the result and the
/// deadline. Concluded jobs answer with where the money actually went.
#[tokio::test]
async fn a_cancel_after_acceptance_bounces_off_committed_work() {
    let (state, _payout) = new_coordinator_state(Duration::from_millis(300));
    let base_url = spawn_coordinator(state.clone()).await;
    let http = reqwest::Client::new();

    let operator = LocalIdentity::generate("operator@e2e");
    let session = http
        .post(format!("{base_url}/federation/operators/register"))
        .json(
            &RegisterRequest::sign(cpu_profile(&operator, 500), payout_addr(1), &operator).unwrap(),
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

    let buyer = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let envelope = signed_envelope(
        &buyer,
        job_id,
        1_000,
        120_000,
        epoch_ms(),
        "e2e-no-clawback",
    );
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let resp = http
        .post(format!("{base_url}/federation/jobs/{job_id}/accept"))
        .bearer_auth(&session)
        .json(&serde_json::json!({ "decision": "accept", "job_id": job_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let cancel_url = format!("{base_url}/federation/jobs/{job_id}/cancel");
    let cancel = |at_ms: u64| {
        covenant_compute_protocol::CancelRequest::sign(buyer.agent_id(), job_id, at_ms, &buyer)
            .unwrap()
    };
    let resp = http
        .post(&cancel_url)
        .json(&cancel(epoch_ms()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body = resp.text().await.unwrap();
    assert!(body.contains("already accepted"), "got: {body}");
    assert_eq!(
        state.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Held,
        "a bounced cancel moves no money"
    );
    assert_eq!(
        state.jobs().get(job_id).unwrap().phase,
        covenant_compute_coordinator::JobPhase::Accepted
    );

    // Concluded phases each answer with the money's real disposition:
    // a paid job points at the dispute path, an already-refunded
    // conclusion has nothing left to cancel.
    state
        .jobs()
        .set_phase(job_id, covenant_compute_coordinator::JobPhase::Completed)
        .unwrap();
    let resp = http
        .post(&cancel_url)
        .json(&cancel(epoch_ms()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body = resp.text().await.unwrap();
    assert!(body.contains("dispute what you were charged for"), "{body}");

    state
        .jobs()
        .set_phase(job_id, covenant_compute_coordinator::JobPhase::Rejected)
        .unwrap();
    let resp = http
        .post(&cancel_url)
        .json(&cancel(epoch_ms()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body = resp.text().await.unwrap();
    assert!(body.contains("nothing to cancel"), "{body}");
}

/// Only the envelope's own buyer, with a fresh and untampered
/// signature, may withdraw a job — every forgery class bounces and
/// none of them moves the job or its money.
#[tokio::test]
async fn only_the_envelopes_buyer_may_cancel_and_forged_cancels_bounce() {
    let (state, _payout) = new_coordinator_state(Duration::from_millis(300));
    let base_url = spawn_coordinator(state.clone()).await;
    let http = reqwest::Client::new();

    let idle_operator = LocalIdentity::generate("idle-operator@e2e");
    let resp = http
        .post(format!("{base_url}/federation/operators/register"))
        .json(
            &RegisterRequest::sign(
                cpu_profile(&idle_operator, 500),
                payout_addr(8),
                &idle_operator,
            )
            .unwrap(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let buyer = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let envelope = signed_envelope(&buyer, job_id, 1_000, 120_000, epoch_ms(), "e2e-forgeries");
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let cancel_url = format!("{base_url}/federation/jobs/{job_id}/cancel");

    // A stranger's signed cancel of someone else's job: 401.
    let stranger = LocalIdentity::generate("stranger@e2e");
    let foreign = covenant_compute_protocol::CancelRequest::sign(
        stranger.agent_id(),
        job_id,
        epoch_ms(),
        &stranger,
    )
    .unwrap();
    let resp = http.post(&cancel_url).json(&foreign).send().await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    // A stale capture: 401 even from the right buyer.
    let stale = covenant_compute_protocol::CancelRequest::sign(
        buyer.agent_id(),
        job_id,
        epoch_ms() - covenant_compute_protocol::CANCEL_MAX_SKEW_MS - 1_000,
        &buyer,
    )
    .unwrap();
    let resp = http.post(&cancel_url).json(&stale).send().await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    // Signed for this job, posted at another job's path: refused
    // before anything is looked up.
    let misrouted = covenant_compute_protocol::CancelRequest::sign(
        buyer.agent_id(),
        job_id,
        epoch_ms(),
        &buyer,
    )
    .unwrap();
    let resp = http
        .post(format!(
            "{base_url}/federation/jobs/{}/cancel",
            Uuid::new_v4()
        ))
        .json(&misrouted)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // Re-dated after signing: the signature no longer verifies.
    let mut redated = covenant_compute_protocol::CancelRequest::sign(
        buyer.agent_id(),
        job_id,
        epoch_ms(),
        &buyer,
    )
    .unwrap();
    redated.cancelled_at_ms = epoch_ms() + 1;
    let resp = http.post(&cancel_url).json(&redated).send().await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body = resp.text().await.unwrap();
    assert!(body.contains("cancellation does not verify"), "got: {body}");

    // An unknown job stays a plain 404.
    let unknown = Uuid::new_v4();
    let ghost = covenant_compute_protocol::CancelRequest::sign(
        buyer.agent_id(),
        unknown,
        epoch_ms(),
        &buyer,
    )
    .unwrap();
    let resp = http
        .post(format!("{base_url}/federation/jobs/{unknown}/cancel"))
        .json(&ghost)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    // None of it moved the job or the money.
    assert_eq!(
        state.jobs().get(job_id).unwrap().phase,
        covenant_compute_coordinator::JobPhase::Offered
    );
    assert_eq!(
        state.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Held
    );
}

/// "Refunded" alone doesn't say why, and the two parties need the why
/// for opposite reasons — the buyer to decide between retrying and
/// re-pricing, the operator to audit its own standing. The rejection
/// path pins the whole read chain: the record's refund_reason, the
/// buyer's receipt poll, and the buyer's history feed all name
/// `operator_rejected`, while a served job's rows carry no reason at
/// all.
#[tokio::test]
async fn a_rejections_reason_reaches_both_parties_reads() {
    let (state, _payout) = new_coordinator_state(Duration::from_millis(300));
    let base_url = spawn_coordinator(state.clone()).await;
    let http = reqwest::Client::new();

    let operator = LocalIdentity::generate("operator@e2e");
    let session = http
        .post(format!("{base_url}/federation/operators/register"))
        .json(
            &RegisterRequest::sign(cpu_profile(&operator, 500), payout_addr(1), &operator).unwrap(),
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

    let buyer = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let envelope = signed_envelope(&buyer, job_id, 1_000, 120_000, epoch_ms(), "e2e-rejected");
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let resp = http
        .post(format!("{base_url}/federation/jobs/{job_id}/accept"))
        .bearer_auth(&session)
        .json(&serde_json::json!({
            "decision": "reject",
            "job_id": job_id,
            "reason": "at capacity",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        state.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Refunded
    );
    let record = state.jobs().get(job_id).unwrap();
    assert_eq!(
        record.phase,
        covenant_compute_coordinator::JobPhase::Rejected
    );
    assert_eq!(
        record.refund_reason,
        Some(covenant_compute_protocol::RefundReason::OperatorRejected)
    );

    // The buyer's per-job poll answers with the reason on the wire.
    let receipt_path = format!("/federation/jobs/{job_id}/receipt");
    let poll_signed_at = epoch_ms();
    let poll_sig =
        covenant_compute_protocol::sign_read(&buyer, &receipt_path, poll_signed_at).unwrap();
    let status: serde_json::Value = http
        .get(format!("{base_url}{receipt_path}"))
        .header(
            covenant_compute_protocol::READ_SIGNED_AT_HEADER,
            poll_signed_at.to_string(),
        )
        .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, poll_sig)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["status"], "rejected");
    assert_eq!(status["refund_reason"], "operator_rejected");

    // And the history feed carries the same answer per row.
    let buyer_key = buyer.agent_id().pubkey_base58();
    let jobs_path = format!("/federation/buyers/{buyer_key}/jobs");
    let feed_signed_at = epoch_ms();
    let feed_sig =
        covenant_compute_protocol::sign_read(&buyer, &jobs_path, feed_signed_at).unwrap();
    let rows: serde_json::Value = http
        .get(format!("{base_url}{jobs_path}"))
        .header(
            covenant_compute_protocol::READ_SIGNED_AT_HEADER,
            feed_signed_at.to_string(),
        )
        .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, feed_sig)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = &rows.as_array().expect("a row array")[0];
    assert_eq!(row["job_id"], job_id.to_string());
    assert_eq!(row["status"], "rejected");
    assert_eq!(row["refund_reason"], "operator_rejected");
}

/// C9's in-process volumetric backstops, over the wire: a capped
/// registry 503s new operators (but never a returning one), and the
/// per-buyer in-flight ceiling 429s the next submission until a job
/// concludes.
#[tokio::test]
async fn volumetric_caps_bound_registrations_and_in_flight_submissions() {
    let identity = LocalIdentity::generate("coordinator@e2e");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let config = CoordinatorConfig {
        long_poll_timeout: Duration::from_secs(2),
        max_operators: Some(1),
        max_inflight_per_buyer: Some(2),
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::new(
        identity,
        config,
        reputation,
        Arc::new(MockPayout::new()),
        audit,
    );
    let base_url = spawn_coordinator(state.clone()).await;
    let http = reqwest::Client::new();
    let resident = LocalIdentity::generate("resident@e2e");
    let resident_req =
        RegisterRequest::sign(cpu_profile(&resident, 1_000), payout_addr(3), &resident).unwrap();
    let resp = http
        .post(format!("{base_url}/federation/operators/register"))
        .json(&resident_req)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // The registry is full: a stranger gets 503, the resident's own
    // restart re-registration still lands.
    let stranger = LocalIdentity::generate("stranger@e2e");
    let stranger_req =
        RegisterRequest::sign(cpu_profile(&stranger, 1_000), payout_addr(4), &stranger).unwrap();
    let resp = http
        .post(format!("{base_url}/federation/operators/register"))
        .json(&stranger_req)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let resp = http
        .post(format!("{base_url}/federation/operators/register"))
        .json(&resident_req)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "re-register at cap");

    // Two in-flight jobs fill the buyer's ceiling; the third bounces
    // with 429 before any hold exists.
    let buyer = LocalIdentity::generate("buyer@e2e");
    let mut job_ids = Vec::new();
    for i in 0..2 {
        let job_id = Uuid::new_v4();
        job_ids.push(job_id);
        let envelope = signed_envelope(
            &buyer,
            job_id,
            1_000,
            30_000,
            epoch_ms(),
            &format!("cap-e2e-{i}"),
        );
        let resp = http
            .post(format!("{base_url}/federation/jobs"))
            .json(&envelope)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    }
    let third = signed_envelope(
        &buyer,
        Uuid::new_v4(),
        1_000,
        30_000,
        epoch_ms(),
        "cap-e2e-3",
    );
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&third)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("in flight"),
        "got: {body}"
    );
    assert!(
        state.escrow().status(third.payload.job_id).await.is_err(),
        "no hold was created for the refused submission"
    );

    // A concluded job frees a slot: reject one in-flight offer, then
    // the same buyer submits again and is admitted.
    state
        .jobs()
        .set_phase(job_ids[0], covenant_compute_coordinator::JobPhase::Rejected)
        .unwrap();
    let fourth = signed_envelope(
        &buyer,
        Uuid::new_v4(),
        1_000,
        30_000,
        epoch_ms(),
        "cap-e2e-4",
    );
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&fourth)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::ACCEPTED,
        "a concluded job frees ceiling room"
    );

    // Another buyer is untouched by this buyer's ceiling.
    let other = LocalIdentity::generate("other-buyer@e2e");
    let other_env = signed_envelope(
        &other,
        Uuid::new_v4(),
        1_000,
        30_000,
        epoch_ms(),
        "cap-e2e-other",
    );
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&other_env)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
}

/// `/metrics` renders the aggregate books as Prometheus text: run one
/// job end to end, dispute it, then scrape and check the figures that
/// a deployment's dashboard would alert on. Everything asserted here
/// is aggregate-only — the scrape must never name a buyer, operator
/// or partner.
#[tokio::test]
async fn metrics_scrape_reports_aggregate_books() {
    let (state, _payout) = new_coordinator_state(Duration::from_secs(5));
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@e2e");
    let profile = cpu_profile(&operator_identity, 1_000);
    let coordinator_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        2,
    ));
    let register_req =
        RegisterRequest::sign(profile.clone(), payout_addr(2), &operator_identity).unwrap();
    assert!(
        coordinator_client
            .register(register_req)
            .await
            .unwrap()
            .accepted
    );

    let operator_pubkey_b58 = operator_identity.agent_id().pubkey_base58();
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

    let buyer_identity = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let envelope = signed_envelope(
        &buyer_identity,
        job_id,
        1_000,
        30_000,
        epoch_ms(),
        "metrics-scrape",
    );
    let http = reqwest::Client::new();
    let submit_resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit_resp.status(), reqwest::StatusCode::ACCEPTED);
    node.run_once()
        .await
        .expect("run_once should succeed")
        .expect("the job must have been offered to this operator");

    let dispute = covenant_compute_protocol::DisputeRequest::sign(
        buyer_identity.agent_id(),
        job_id,
        "metrics test dispute".into(),
        epoch_ms(),
        &buyer_identity,
    )
    .unwrap();
    let resp = http
        .post(format!("{base_url}/federation/jobs/{job_id}/dispute"))
        .json(&dispute)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let scrape = http
        .get(format!("{base_url}/metrics"))
        .send()
        .await
        .unwrap();
    assert_eq!(scrape.status(), reqwest::StatusCode::OK);
    assert_eq!(
        scrape
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/plain; version=0.0.4")
    );
    let body = scrape.text().await.unwrap();

    let build_info = format!(
        "compute_build_info{{version=\"{}\"}} 1",
        env!("CARGO_PKG_VERSION")
    );
    let protocol_version = format!(
        "compute_protocol_version {}",
        covenant_compute_protocol::PROTOCOL_VERSION
    );
    for line in [
        build_info.as_str(),
        protocol_version.as_str(),
        "compute_protocol_min_supported 0",
        "compute_operators_registered 1",
        "compute_operators_live 1",
        "compute_jobs{phase=\"offered\"} 0",
        "compute_jobs{phase=\"completed\"} 1",
        "compute_jobs_disputed 1",
        "compute_fee_bps 0",
        "compute_fees_captured_micro_usdc_total 0",
        "compute_subsidy_enforced 0",
        "compute_subsidy_closed 0",
        "compute_prefunding_enforced 0",
        // The money books: one open-mode job completed at 1_000 with a
        // zero fee — released gross equals the pushed payout, and the
        // buyer-facing books are empty because nothing was prefunded.
        "compute_deposited_micro_usdc_total 0",
        "compute_withdrawn_micro_usdc_total 0",
        "compute_buyer_available_micro_usdc 0",
        "compute_escrow_held_micro_usdc{funding_source=\"organic\"} 0",
        "compute_escrow_released_micro_usdc_total{funding_source=\"organic\"} 1000",
        "compute_escrow_refunded_micro_usdc_total{funding_source=\"organic\"} 0",
        "compute_payouts_pushed_micro_usdc_total 1000",
        "compute_payouts_outstanding_micro_usdc 0",
        // released 1000 = pushed 1000 + owed 0 + fees 0: the books balance.
        "compute_reconciliation_drift_micro_usdc 0",
        "compute_partner_accrued_micro_usdc_total 0",
        "compute_partner_paid_micro_usdc_total 0",
        "compute_bonds_posted_micro_usdc_total 0",
        "compute_bonds_slashed_micro_usdc_total 0",
        "compute_bonds_refunded_micro_usdc_total 0",
        "compute_bonds_at_stake_micro_usdc 0",
        "compute_bonds_unbonding_micro_usdc 0",
    ] {
        assert!(
            body.lines().any(|l| l == line),
            "missing metric line {line:?} in scrape:\n{body}"
        );
    }
    assert!(
        !body.contains(&operator_pubkey_b58),
        "the scrape must stay aggregate-only, but it names an operator:\n{body}"
    );
}

#[tokio::test]
async fn a_buyer_withdraws_unspent_balance_and_the_books_hold() {
    use covenant_compute_coordinator::{MockRail, VerifiedDeposit};

    let dir = tempfile::tempdir().unwrap();
    let identity_path = dir.path().join("identity.json");
    let journal_path = dir.path().join("journal.jsonl");
    let rail = Arc::new(MockRail::new());
    let payout = Arc::new(MockPayout::new());

    let make_state = |rail: Arc<MockRail>, payout: Arc<MockPayout>| {
        let identity =
            LocalIdentity::load_or_create(&identity_path, "coordinator@withdraw").unwrap();
        let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
        let journal_path = journal_path.clone();
        async move {
            CoordinatorState::with_journal(
                identity,
                CoordinatorConfig {
                    long_poll_timeout: Duration::from_secs(2),
                    default_funding_source: FundingSource::Organic,
                    require_prefunded_buyers: true,
                    ..CoordinatorConfig::default()
                },
                Arc::new(AuditReputationSource::new(audit.clone())),
                payout,
                audit,
                &journal_path,
                Some(rail),
            )
            .await
            .unwrap()
        }
    };

    let state = make_state(rail.clone(), payout.clone()).await;
    let (base_url, server) = spawn_coordinator_abortable(state.clone()).await;

    let buyer_identity = LocalIdentity::generate("buyer@withdraw");
    let buyer_key = buyer_identity.agent_id().pubkey_base58();
    let http = reqwest::Client::new();
    let buyer_config = covenant_compute_buyer::BuyerConfig {
        coordinator_url: base_url.clone(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    // A real 32-byte owner address for the transfer's destination.
    let wallet = LocalIdentity::generate("wallet@withdraw")
        .agent_id()
        .pubkey_base58();

    rail.preload(VerifiedDeposit {
        deposit_id: "sig-withdraw-1".into(),
        buyer_pubkey_b58: buyer_key.clone(),
        amount_micro_usdc: 10_000,
    });
    covenant_compute_buyer::claim_deposit(&http, &buyer_config, &buyer_identity, "sig-withdraw-1")
        .await
        .unwrap();

    // Overdrawing the balance is refused before anything moves.
    let overdraw = covenant_compute_buyer::withdraw(
        &http,
        &buyer_config,
        &buyer_identity,
        Uuid::new_v4(),
        12_000,
        &wallet,
    )
    .await
    .expect_err("overdraw must be refused");
    assert!(
        overdraw.to_string().contains("402"),
        "overdraw should 402, got: {overdraw}"
    );

    // A covered withdrawal debits and (mock backend) records the
    // intended transfer, memo-linked to the withdrawal id.
    let withdrawal_id = Uuid::new_v4();
    let view = covenant_compute_buyer::withdraw(
        &http,
        &buyer_config,
        &buyer_identity,
        withdrawal_id,
        4_000,
        &wallet,
    )
    .await
    .unwrap();
    assert_eq!(view.withdrawal_id, withdrawal_id);
    assert!(view.pushed, "mock backend pushes inline");
    assert_eq!(view.tx_signature, None, "mock backend submits nothing");
    assert_eq!(
        view.memo,
        covenant_compute_protocol::withdrawal_memo_for(&buyer_key, withdrawal_id)
    );
    let transfers = payout.transfers();
    assert_eq!(transfers.len(), 1);
    assert_eq!(transfers[0].recipient_address, wallet);
    assert_eq!(transfers[0].amount_micro_usdc, 4_000);

    // An honest retry of the same id acknowledges without re-debiting
    // or re-paying.
    let retry = covenant_compute_buyer::withdraw(
        &http,
        &buyer_config,
        &buyer_identity,
        withdrawal_id,
        4_000,
        &wallet,
    )
    .await
    .unwrap();
    assert_eq!(retry.withdrawal_id, withdrawal_id);
    assert_eq!(payout.transfers().len(), 1, "no second transfer");

    // The balance view carries the debit; a job the remainder can't
    // cover is refused at submit.
    let funds =
        covenant_compute_buyer::funds_with_deposit_info(&http, &buyer_config, &buyer_identity)
            .await
            .unwrap();
    assert_eq!(funds["balance"]["withdrawn_micro_usdc"], 4_000);
    assert_eq!(funds["balance"]["available_micro_usdc"], 6_000);
    let refused = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&signed_envelope(
            &buyer_identity,
            Uuid::new_v4(),
            7_000,
            30_000,
            epoch_ms(),
            "withdraw-overdraft-job",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::PAYMENT_REQUIRED);

    // Withdrawing more than remains is refused the same way — a
    // payment problem naming both numbers — and pays no one.
    let overdraw = covenant_compute_protocol::WithdrawalRequest::sign(
        buyer_identity.agent_id(),
        Uuid::new_v4(),
        7_000,
        wallet.clone(),
        epoch_ms(),
        &buyer_identity,
    )
    .unwrap();
    let resp = http
        .post(format!("{base_url}/federation/buyers/withdraw"))
        .json(&overdraw)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::PAYMENT_REQUIRED);
    let text = resp.text().await.unwrap();
    assert!(
        text.contains("withdrawal refused: 7000 micro-USDC requested, 6000 available"),
        "got: {text}"
    );
    assert_eq!(payout.transfers().len(), 1, "an overdraw pays no one");

    // The history is a signed read: bare curiosity gets 401, the buyer
    // sees the row.
    let unsigned = http
        .get(format!(
            "{base_url}/federation/buyers/{buyer_key}/withdrawals"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(unsigned.status(), 401);
    let rows = covenant_compute_buyer::list_withdrawals(&http, &buyer_config, &buyer_identity)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].pushed);

    // A stale-but-honest signature is refused before the books move:
    // the skew window is a replay defense, so a captured withdrawal
    // request that would otherwise debit gets 401 from the buyer's own
    // key, and no second transfer leaves.
    let stale = covenant_compute_protocol::WithdrawalRequest::sign(
        buyer_identity.agent_id(),
        Uuid::new_v4(),
        1_000,
        wallet.clone(),
        epoch_ms() - covenant_compute_protocol::WITHDRAWAL_MAX_SKEW_MS - 1_000,
        &buyer_identity,
    )
    .unwrap();
    let resp = http
        .post(format!("{base_url}/federation/buyers/withdraw"))
        .json(&stale)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(
        payout.transfers().len(),
        1,
        "a stale withdrawal pays no one"
    );

    // A relay that re-points the recipient after signing is refused:
    // the recipient is inside the signed payload, so the tampered
    // request no longer verifies and the money stays put.
    let thief = LocalIdentity::generate("thief@withdraw")
        .agent_id()
        .pubkey_base58();
    let mut repointed = covenant_compute_protocol::WithdrawalRequest::sign(
        buyer_identity.agent_id(),
        Uuid::new_v4(),
        1_000,
        wallet.clone(),
        epoch_ms(),
        &buyer_identity,
    )
    .unwrap();
    repointed.recipient_address_b58 = thief;
    let resp = http
        .post(format!("{base_url}/federation/buyers/withdraw"))
        .json(&repointed)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(resp
        .text()
        .await
        .unwrap()
        .contains("withdrawal does not verify"));
    assert_eq!(
        payout.transfers().len(),
        1,
        "a re-pointed withdrawal pays no one"
    );

    // The debit survives a coordinator restart: the journal replays it
    // and the derived balance still excludes the withdrawn amount.
    server.abort();
    let _ = server.await;
    let state2 = make_state(rail, Arc::new(MockPayout::new())).await;
    let (base_url2, _server2) = spawn_coordinator_abortable(state2).await;
    let buyer_config2 = covenant_compute_buyer::BuyerConfig {
        coordinator_url: base_url2,
        ..buyer_config
    };
    let funds =
        covenant_compute_buyer::funds_with_deposit_info(&http, &buyer_config2, &buyer_identity)
            .await
            .unwrap();
    assert_eq!(funds["balance"]["withdrawn_micro_usdc"], 4_000);
    assert_eq!(funds["balance"]["available_micro_usdc"], 6_000);
    let rows = covenant_compute_buyer::list_withdrawals(&http, &buyer_config2, &buyer_identity)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "the pushed withdrawal replays whole");
    assert!(rows[0].pushed, "the push outcome is journaled, not re-run");
}

#[tokio::test]
async fn an_over_obligation_cap_withdrawal_is_refused_up_front_and_takes_no_debit() {
    use covenant_compute_coordinator::{MockRail, VerifiedDeposit};

    let dir = tempfile::tempdir().unwrap();
    let identity_path = dir.path().join("identity.json");
    let journal_path = dir.path().join("journal.jsonl");
    let rail = Arc::new(MockRail::new());
    // A deployment that opts into bounding a single obligation transfer.
    let payout = Arc::new(MockPayout::with_obligation_cap(50_000));

    let identity = LocalIdentity::load_or_create(&identity_path, "coordinator@oblig-cap").unwrap();
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let state = CoordinatorState::with_journal(
        identity,
        CoordinatorConfig {
            long_poll_timeout: Duration::from_secs(2),
            default_funding_source: FundingSource::Organic,
            require_prefunded_buyers: true,
            ..CoordinatorConfig::default()
        },
        Arc::new(AuditReputationSource::new(audit.clone())),
        payout.clone(),
        audit,
        &journal_path,
        Some(rail.clone()),
    )
    .await
    .unwrap();
    let (base_url, _server) = spawn_coordinator_abortable(state).await;

    let buyer_identity = LocalIdentity::generate("buyer@oblig-cap");
    let buyer_key = buyer_identity.agent_id().pubkey_base58();
    let http = reqwest::Client::new();
    let buyer_config = covenant_compute_buyer::BuyerConfig {
        coordinator_url: base_url.clone(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    let wallet = LocalIdentity::generate("wallet@oblig-cap")
        .agent_id()
        .pubkey_base58();

    // Fund the buyer with more than the per-transfer cap.
    rail.preload(VerifiedDeposit {
        deposit_id: "sig-oblig-1".into(),
        buyer_pubkey_b58: buyer_key.clone(),
        amount_micro_usdc: 90_000,
    });
    covenant_compute_buyer::claim_deposit(&http, &buyer_config, &buyer_identity, "sig-oblig-1")
        .await
        .unwrap();

    // A withdrawal above the cap is refused before any debit — the old
    // bug took the debit and then spun the sweep on a push it could
    // never complete. A 400 naming the cap is the whole fix.
    let over_cap = covenant_compute_protocol::WithdrawalRequest::sign(
        buyer_identity.agent_id(),
        Uuid::new_v4(),
        90_000,
        wallet.clone(),
        epoch_ms(),
        &buyer_identity,
    )
    .unwrap();
    let resp = http
        .post(format!("{base_url}/federation/buyers/withdraw"))
        .json(&over_cap)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let text = resp.text().await.unwrap();
    assert!(
        text.contains("per-transfer obligation cap"),
        "the refusal must name the cap; got: {text}"
    );

    // Nothing was debited and nothing was pushed: the balance is whole.
    assert!(
        payout.transfers().is_empty(),
        "an over-cap request pays no one"
    );
    let funds =
        covenant_compute_buyer::funds_with_deposit_info(&http, &buyer_config, &buyer_identity)
            .await
            .unwrap();
    assert_eq!(funds["balance"]["withdrawn_micro_usdc"], 0);
    assert_eq!(funds["balance"]["available_micro_usdc"], 90_000);

    // At the cap it clears: the full balance stays reachable, just in
    // transfers no larger than the cap.
    let at_cap_id = Uuid::new_v4();
    let view = covenant_compute_buyer::withdraw(
        &http,
        &buyer_config,
        &buyer_identity,
        at_cap_id,
        50_000,
        &wallet,
    )
    .await
    .unwrap();
    assert_eq!(view.withdrawal_id, at_cap_id);
    assert!(view.pushed);
    assert_eq!(payout.transfers().len(), 1);
    assert_eq!(payout.transfers()[0].amount_micro_usdc, 50_000);
}

#[tokio::test]
async fn a_streaming_job_relays_chunks_to_its_buyer_and_nobody_else() {
    use covenant_compute_protocol::{StreamChunk, StreamPush};

    let (state, _payout) = new_coordinator_state(Duration::from_secs(2));
    let base_url = spawn_coordinator(state).await;
    let http = reqwest::Client::new();

    // Register raw so the test holds the session token itself — chunk
    // pushes are session-authed and this test exercises that boundary.
    let operator_identity = LocalIdentity::generate("operator@stream-e2e");
    let register = http
        .post(format!("{base_url}/federation/operators/register"))
        .json(
            &RegisterRequest::sign(
                cpu_profile(&operator_identity, 1_000),
                payout_addr(1),
                &operator_identity,
            )
            .unwrap(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(register.status(), reqwest::StatusCode::OK);
    let session = register.json::<serde_json::Value>().await.unwrap()["operator_session"]
        .as_str()
        .unwrap()
        .to_string();

    let buyer_identity = LocalIdentity::generate("buyer@stream-e2e");
    let job_id = Uuid::new_v4();
    let mut payload = JobEnvelopePayload {
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
        input: vec![Content::text("stream me")],
        price_micro_usdc: 1_000,
        deadline_ms: 30_000,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "e2e-stream"),
        issued_at_ms: epoch_ms(),
        referral_code: None,
        stream: true,
    };
    let envelope = SignedJobEnvelope::sign(payload.clone(), &buyer_identity).unwrap();
    let submit = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit.status(), reqwest::StatusCode::ACCEPTED);

    // Offered but not yet accepted: chunks only flow while the job
    // runs, and the refusal names the phase it is actually in.
    let early = http
        .post(format!("{base_url}/federation/jobs/{job_id}/stream"))
        .bearer_auth(&session)
        .json(&StreamPush {
            job_id,
            chunks: vec![StreamChunk {
                seq: 0,
                text: "eager".into(),
            }],
            done: false,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(early.status(), reqwest::StatusCode::CONFLICT);
    assert!(early
        .text()
        .await
        .unwrap()
        .contains("chunks are accepted only while it runs"));

    // A decision for this job aimed at another job's accept path is
    // refused before the session is even checked — the path and the
    // decision have to name the same job, so an accept can't be
    // misrouted onto a job it never named.
    let misrouted = http
        .post(format!(
            "{base_url}/federation/jobs/{}/accept",
            Uuid::new_v4()
        ))
        .bearer_auth(&session)
        .json(&covenant_compute_protocol::JobAccept::Accept { job_id })
        .send()
        .await
        .unwrap();
    assert_eq!(misrouted.status(), reqwest::StatusCode::BAD_REQUEST);

    let accept = http
        .post(format!("{base_url}/federation/jobs/{job_id}/accept"))
        .bearer_auth(&session)
        .json(&covenant_compute_protocol::JobAccept::Accept { job_id })
        .send()
        .await
        .unwrap();
    assert_eq!(accept.status(), reqwest::StatusCode::OK);

    let stream_url = format!("{base_url}/federation/jobs/{job_id}/stream");
    let batch_one = StreamPush {
        job_id,
        chunks: vec![
            StreamChunk {
                seq: 0,
                text: "fifty".into(),
            },
            StreamChunk {
                seq: 1,
                text: "-".into(),
            },
        ],
        done: false,
    };

    // No session: refused before a byte lands.
    let unauthed = http
        .post(&stream_url)
        .json(&batch_one)
        .send()
        .await
        .unwrap();
    assert_eq!(unauthed.status(), reqwest::StatusCode::UNAUTHORIZED);

    // A different registered operator's live session must not open
    // someone else's job stream.
    let impostor_identity = LocalIdentity::generate("impostor@stream-e2e");
    let impostor_register = http
        .post(format!("{base_url}/federation/operators/register"))
        .json(
            &RegisterRequest::sign(
                cpu_profile(&impostor_identity, 1_000),
                payout_addr(1),
                &impostor_identity,
            )
            .unwrap(),
        )
        .send()
        .await
        .unwrap();
    let impostor_session = impostor_register.json::<serde_json::Value>().await.unwrap()
        ["operator_session"]
        .as_str()
        .unwrap()
        .to_string();
    let foreign = http
        .post(&stream_url)
        .bearer_auth(&impostor_session)
        .json(&batch_one)
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), reqwest::StatusCode::UNAUTHORIZED);

    // A push whose body names this job but whose path names another is
    // refused before the job is even looked up — a 400, not the 404
    // the unknown path would earn — so chunks can't be misrouted onto
    // a stream they never named.
    let misrouted = http
        .post(format!(
            "{base_url}/federation/jobs/{}/stream",
            Uuid::new_v4()
        ))
        .bearer_auth(&session)
        .json(&batch_one)
        .send()
        .await
        .unwrap();
    assert_eq!(misrouted.status(), reqwest::StatusCode::BAD_REQUEST);

    // The assigned operator's push lands; a verbatim retry is safe.
    for _ in 0..2 {
        let push = http
            .post(&stream_url)
            .bearer_auth(&session)
            .json(&batch_one)
            .send()
            .await
            .unwrap();
        assert_eq!(push.status(), reqwest::StatusCode::OK);
    }

    // A gap is refused — the buffer only ever serves a true prefix.
    let gapped = http
        .post(&stream_url)
        .bearer_auth(&session)
        .json(&StreamPush {
            job_id,
            chunks: vec![StreamChunk {
                seq: 5,
                text: "hole".into(),
            }],
            done: false,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(gapped.status(), reqwest::StatusCode::CONFLICT);

    // The buyer reads with a signed read, exactly like the receipt poll.
    let stream_path = format!("/federation/jobs/{job_id}/stream");
    let signed_read = |since: u64| {
        let signed_at_ms = epoch_ms();
        let signature =
            covenant_compute_protocol::sign_read(&buyer_identity, &stream_path, signed_at_ms)
                .unwrap();
        http.get(format!("{base_url}{stream_path}?since={since}"))
            .header(
                covenant_compute_protocol::READ_SIGNED_AT_HEADER,
                signed_at_ms.to_string(),
            )
            .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, signature)
            .send()
    };

    let unsigned = http
        .get(format!("{base_url}{stream_path}"))
        .send()
        .await
        .unwrap();
    assert_eq!(unsigned.status(), reqwest::StatusCode::UNAUTHORIZED);

    // A captured read replayed after the skew window is refused even
    // from the buyer's own key — chunk text is priced output, so a
    // stale signature must not reopen it.
    let stale_at_ms = epoch_ms() - covenant_compute_protocol::SIGNED_READ_MAX_SKEW_MS - 1_000;
    let stale_sig =
        covenant_compute_protocol::sign_read(&buyer_identity, &stream_path, stale_at_ms).unwrap();
    let stale = http
        .get(format!("{base_url}{stream_path}"))
        .header(
            covenant_compute_protocol::READ_SIGNED_AT_HEADER,
            stale_at_ms.to_string(),
        )
        .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, stale_sig)
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(stale
        .text()
        .await
        .unwrap()
        .contains("read signature rejected"));

    // A signed-at header that isn't a timestamp never reaches the
    // signature check.
    let garbled = http
        .get(format!("{base_url}{stream_path}"))
        .header(
            covenant_compute_protocol::READ_SIGNED_AT_HEADER,
            "yesterday",
        )
        .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, "sig")
        .send()
        .await
        .unwrap();
    assert_eq!(garbled.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(garbled
        .text()
        .await
        .unwrap()
        .contains("must be epoch milliseconds"));

    let view = signed_read(0).await.unwrap();
    assert_eq!(view.status(), reqwest::StatusCode::OK);
    let view: serde_json::Value = view.json().await.unwrap();
    assert_eq!(view["status"], "accepted");
    assert_eq!(view["next_seq"], 2);
    assert_eq!(view["done"], false);
    assert_eq!(view["chunks"][0]["text"], "fifty");
    assert_eq!(view["chunks"][1]["text"], "-");

    let tail: serde_json::Value = signed_read(1).await.unwrap().json().await.unwrap();
    assert_eq!(tail["chunks"].as_array().unwrap().len(), 1);
    assert_eq!(tail["chunks"][0]["seq"], 1);

    // The final batch closes the feed.
    let push = http
        .post(&stream_url)
        .bearer_auth(&session)
        .json(&StreamPush {
            job_id,
            chunks: vec![StreamChunk {
                seq: 2,
                text: "five".into(),
            }],
            done: true,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(push.status(), reqwest::StatusCode::OK);

    // The receipt settles the job as usual, chunks or no chunks.
    let output = vec![Content::text("fifty-five")];
    let receipt = SignedWorkReceipt::sign(
        WorkReceiptPayload {
            job_id,
            operator: operator_identity.agent_id(),
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
            status: A2ATaskStatus::Ok,
            executed_at_ms: epoch_ms(),
            node_audit_root_hex: "cc".repeat(32),
        },
        &operator_identity,
    )
    .unwrap();
    let result = http
        .post(format!("{base_url}/federation/jobs/{job_id}/result"))
        .json(&covenant_compute_protocol::JobResultMessage { receipt, output })
        .send()
        .await
        .unwrap();
    assert_eq!(result.status(), reqwest::StatusCode::OK);

    // A batch racing in after conclusion is acknowledged and dropped.
    let late = http
        .post(&stream_url)
        .bearer_auth(&session)
        .json(&StreamPush {
            job_id,
            chunks: vec![StreamChunk {
                seq: 3,
                text: "late".into(),
            }],
            done: false,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(late.status(), reqwest::StatusCode::OK);

    let final_view: serde_json::Value = signed_read(0).await.unwrap().json().await.unwrap();
    assert_eq!(final_view["status"], "completed");
    assert_eq!(final_view["done"], true);
    assert_eq!(final_view["next_seq"], 3, "the late chunk was dropped");
    let streamed: String = final_view["chunks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        streamed, "fifty-five",
        "assembled feed equals the paid output"
    );

    // A job that never asked for streaming refuses pushes outright.
    let plain_job = Uuid::new_v4();
    payload.job_id = plain_job;
    payload.stream = false;
    payload.idempotency = A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "e2e-no-stream");
    let plain_envelope = SignedJobEnvelope::sign(payload, &buyer_identity).unwrap();
    let submit = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&plain_envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit.status(), reqwest::StatusCode::ACCEPTED);
    let accept = http
        .post(format!("{base_url}/federation/jobs/{plain_job}/accept"))
        .bearer_auth(&session)
        .json(&covenant_compute_protocol::JobAccept::Accept { job_id: plain_job })
        .send()
        .await
        .unwrap();
    assert_eq!(accept.status(), reqwest::StatusCode::OK);
    let refused = http
        .post(format!("{base_url}/federation/jobs/{plain_job}/stream"))
        .bearer_auth(&session)
        .json(&StreamPush {
            job_id: plain_job,
            chunks: vec![StreamChunk {
                seq: 0,
                text: "unasked".into(),
            }],
            done: false,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_streaming_buyer_drains_the_feed_and_verifies_the_same_receipt() {
    let (state, _payout) = new_coordinator_state(Duration::from_secs(5));
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@stream-buyer-e2e");
    let profile = cpu_profile(&operator_identity, 1_000);
    let coordinator_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        2,
    ));
    let register_req =
        RegisterRequest::sign(profile.clone(), payout_addr(1), &operator_identity).unwrap();
    assert!(
        coordinator_client
            .register(register_req)
            .await
            .unwrap()
            .accepted
    );

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
    // The node long-polls first so the offer lands the moment the
    // streaming dispatch below submits it.
    let node_task = tokio::spawn(async move { node.run_once().await });

    let buyer_identity = LocalIdentity::generate("buyer@stream-buyer-e2e");
    let config = covenant_compute_buyer::BuyerConfig {
        coordinator_url: base_url,
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    let http = reqwest::Client::new();
    let mut live_chunks: Vec<String> = Vec::new();
    let streamed = covenant_compute_buyer::dispatch_streaming(
        &http,
        &config,
        &buyer_identity,
        covenant_compute_buyer::JobRequest {
            kind: JobKind::BatchJob,
            input: vec![Content::text("fifty-"), Content::text("five")],
            model: None,
            gpu_class: None,
            min_vram_gb: None,
            min_reputation_bps: None,
            price_micro_usdc: 1_000,
            deadline_ms: 30_000,
        },
        |chunk| live_chunks.push(chunk.to_string()),
    )
    .await
    .expect("streaming dispatch verifies");

    assert!(!live_chunks.is_empty(), "the feed delivered chunks");
    assert_eq!(live_chunks.concat(), "fifty-five");
    assert!(
        streamed.stream_matched_output,
        "assembled feed equals the receipt-verified output"
    );
    assert_eq!(
        streamed.outcome.output,
        vec![Content::text("fifty-"), Content::text("five")]
    );
    assert_eq!(
        streamed.outcome.receipt.receipt.status,
        A2ATaskStatus::Ok,
        "the money path concluded exactly as a non-streaming job"
    );

    let node_outcome = node_task
        .await
        .unwrap()
        .expect("run_once should succeed")
        .expect("the job was offered");
    assert!(node_outcome.error_message.is_none());
}

/// The whole bond lifecycle over the real HTTP surface (C5 phase 2): a
/// rail-verified post credits stake and lifts the operator over the
/// matching floor, a coordinator-proven fault slashes the stake while
/// an unbond matures, the matured refund pays exactly the remainder,
/// and every book survives a coordinator restart — including the
/// claim's idempotency id.
#[tokio::test]
async fn an_operator_bonds_gets_slashed_mid_exit_and_is_refunded_the_remainder() {
    use covenant_compute_coordinator::{sweep::sweep_matured_unbonds, MockRail, VerifiedBond};
    use covenant_compute_protocol::UnbondRequest;

    let dir = tempfile::tempdir().unwrap();
    let identity_path = dir.path().join("identity.json");
    let journal_path = dir.path().join("journal.jsonl");
    let rail = Arc::new(MockRail::new());
    let payout = Arc::new(MockPayout::new());

    let bonded_state = |rail: Arc<MockRail>, payout: Arc<MockPayout>| {
        let identity = LocalIdentity::load_or_create(&identity_path, "coordinator@bonded").unwrap();
        let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
        let journal_path = journal_path.clone();
        async move {
            CoordinatorState::with_journal(
                identity,
                CoordinatorConfig {
                    long_poll_timeout: Duration::from_secs(2),
                    default_funding_source: FundingSource::Organic,
                    min_bond_micro_usdc: 50_000,
                    // Matured on the next sweep tick — the window's length
                    // is a deployment knob, not what this test proves.
                    unbond_window: Duration::ZERO,
                    ..CoordinatorConfig::default()
                },
                Arc::new(AuditReputationSource::new(audit.clone())),
                payout,
                audit,
                &journal_path,
                Some(rail),
            )
            .await
            .unwrap()
        }
    };

    let state = bonded_state(rail.clone(), payout.clone()).await;
    let state_handle = state.clone();
    let base_url = spawn_coordinator(state).await;
    let http = reqwest::Client::new();

    let operator = LocalIdentity::generate("operator@bonded");
    let operator_key = operator.agent_id().pubkey_base58();
    let coordinator_client =
        HttpCoordinatorClient::with_config(base_url.clone(), Duration::from_secs(2), 1);
    coordinator_client
        .register(
            RegisterRequest::sign(cpu_profile(&operator, 1_000), payout_addr(5), &operator)
                .unwrap(),
        )
        .await
        .unwrap();

    // Registered but unbonded: below the floor, the operator wins
    // nothing — the buyer is refunded, not matched to unstaked hardware.
    let buyer = LocalIdentity::generate("buyer@bonded");
    let unmatched = Uuid::new_v4();
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&signed_envelope(
            &buyer,
            unmatched,
            1_000,
            30_000,
            epoch_ms(),
            "bond-floor-unmatched",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);

    // The public bond-info names the floor and the memo shape.
    let info: serde_json::Value = http
        .get(format!("{base_url}/federation/bond-info"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["configured"], true);
    assert_eq!(info["min_bond_micro_usdc"], 50_000);
    assert!(info["memo_format"]
        .as_str()
        .unwrap()
        .starts_with("compute-bond:v1:"));

    // A post the rail never saw verifies nothing; a confirmed post
    // claimed for someone else credits the memo's operator, never the
    // claimant.
    let resp = http
        .post(format!("{base_url}/federation/operators/bond"))
        .json(&serde_json::json!({
            "operator_pubkey_b58": operator_key,
            "bond_id": "sig-nowhere",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    rail.preload_bond(VerifiedBond {
        bond_id: "sig-bond-1".into(),
        operator_pubkey_b58: operator_key.clone(),
        amount_micro_usdc: 60_000,
    });
    let resp = http
        .post(format!("{base_url}/federation/operators/bond"))
        .json(&serde_json::json!({
            "operator_pubkey_b58": "someone-else",
            "bond_id": "sig-bond-1",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // The rightful claim credits once; the retry acknowledges without
    // crediting twice.
    let claim = serde_json::json!({
        "operator_pubkey_b58": operator_key,
        "bond_id": "sig-bond-1",
    });
    let credited: serde_json::Value = http
        .post(format!("{base_url}/federation/operators/bond"))
        .json(&claim)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(credited["credited"], true);
    assert_eq!(credited["posted_total_micro_usdc"], 60_000);
    let replay: serde_json::Value = http
        .post(format!("{base_url}/federation/operators/bond"))
        .json(&claim)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay["credited"], false);
    assert_eq!(replay["posted_total_micro_usdc"], 60_000);

    // Over the floor, the same operator now wins the job.
    let matched = Uuid::new_v4();
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&signed_envelope(
            &buyer,
            matched,
            1_000,
            30_000,
            epoch_ms(),
            "bond-floor-matched",
        ))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "bonded: {}", resp.status());

    // A coordinator-proven fault takes the job price from the stake.
    state_handle
        .slash_for_fault(
            "canary",
            Uuid::new_v4(),
            &operator_key,
            10_000,
            "canary wrong-answer: e2e",
        )
        .await;

    // The bond feed is a signed read: bare curiosity gets 401, the
    // operator's own signature reads the whole stake picture.
    let feed_path = format!("/federation/operators/{operator_key}/bond");
    let resp = http
        .get(format!("{base_url}{feed_path}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    let signed_at = epoch_ms();
    let sig = covenant_compute_protocol::sign_read(&operator, &feed_path, signed_at).unwrap();
    let feed: serde_json::Value = http
        .get(format!("{base_url}{feed_path}"))
        .header(
            covenant_compute_protocol::READ_SIGNED_AT_HEADER,
            signed_at.to_string(),
        )
        .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, sig)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(feed["status"]["posted_micro_usdc"], 60_000);
    assert_eq!(feed["status"]["slashed_micro_usdc"], 10_000);
    assert_eq!(feed["status"]["committed_micro_usdc"], 50_000);
    assert_eq!(feed["slashes"][0]["reason"], "canary wrong-answer: e2e");

    // The operator heads for the exit with everything still standing.
    let exit_wallet = LocalIdentity::generate("wallet@bonded")
        .agent_id()
        .pubkey_base58();
    // A stale-but-honest exit is refused before the stake moves: the
    // same 50_000 that unbonds cleanly with a fresh timestamp gets 401
    // from the operator's own key when the signature is outside the
    // window — the skew guard runs ahead of the committed-funds check.
    let stale = UnbondRequest::sign(
        operator.agent_id(),
        Uuid::new_v4(),
        50_000,
        exit_wallet.clone(),
        epoch_ms() - covenant_compute_protocol::UNBOND_MAX_SKEW_MS - 1_000,
        &operator,
    )
    .unwrap();
    let resp = http
        .post(format!("{base_url}/federation/operators/unbond"))
        .json(&stale)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    // A relay that re-points the refund recipient after signing is
    // refused: the recipient is inside the signed payload, so the
    // tampered request no longer verifies and the stake stays
    // committed — the honest 50_000 exit below still goes through.
    let thief = LocalIdentity::generate("thief@bonded")
        .agent_id()
        .pubkey_base58();
    let mut repointed = UnbondRequest::sign(
        operator.agent_id(),
        Uuid::new_v4(),
        50_000,
        exit_wallet.clone(),
        epoch_ms(),
        &operator,
    )
    .unwrap();
    repointed.recipient_address_b58 = thief;
    let resp = http
        .post(format!("{base_url}/federation/operators/unbond"))
        .json(&repointed)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(resp
        .text()
        .await
        .unwrap()
        .contains("unbond does not verify"));

    let unbond = UnbondRequest::sign(
        operator.agent_id(),
        Uuid::new_v4(),
        50_000,
        exit_wallet.clone(),
        epoch_ms(),
        &operator,
    )
    .unwrap();
    let requested: serde_json::Value = http
        .post(format!("{base_url}/federation/operators/unbond"))
        .json(&unbond)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(requested["pushed"], false);
    assert!(requested["memo"]
        .as_str()
        .unwrap()
        .starts_with("compute-bond-refund:v1:"));

    // Nothing left to commit: a second request overdraws and the
    // operator is already unmatchable again — the exit shut the door
    // before the money moved.
    let overdraw = UnbondRequest::sign(
        operator.agent_id(),
        Uuid::new_v4(),
        1,
        exit_wallet.clone(),
        epoch_ms(),
        &operator,
    )
    .unwrap();
    let resp = http
        .post(format!("{base_url}/federation/operators/unbond"))
        .json(&overdraw)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::PAYMENT_REQUIRED);
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&signed_envelope(
            &buyer,
            Uuid::new_v4(),
            1_000,
            30_000,
            epoch_ms(),
            "bond-floor-exiting",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);

    // The matured refund pays the remainder to the signed recipient.
    let concluded = sweep_matured_unbonds(&state_handle).await;
    assert_eq!(concluded, vec![unbond.unbond_id]);
    let transfers = payout.transfers();
    assert_eq!(transfers.len(), 1);
    assert_eq!(transfers[0].amount_micro_usdc, 50_000);
    assert_eq!(transfers[0].recipient_address, exit_wallet);

    // Restart: posted, slashed, refunded and the claim's idempotency
    // id all come back from the journal.
    let restarted = bonded_state(rail.clone(), Arc::new(MockPayout::new())).await;
    let status = restarted.bonds().status(&operator_key);
    assert_eq!(status.posted_micro_usdc, 60_000);
    assert_eq!(status.slashed_micro_usdc, 10_000);
    assert_eq!(status.refunded_micro_usdc, 50_000);
    assert_eq!(status.at_stake_micro_usdc, 0);
    let base_url = spawn_coordinator(restarted).await;
    let replay: serde_json::Value = http
        .post(format!("{base_url}/federation/operators/bond"))
        .json(&claim)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        replay["credited"], false,
        "the dedup id survived the restart"
    );
}

/// The routing heal, end to end over real HTTP: the matcher's first
/// pick registers and then never polls (a node that died holding an
/// offer), so past the re-offer window the sweep re-points the job at
/// the live sibling, which serves it and is the one paid. The vanished
/// assignee's late accept bounces off the assignee guard, and nothing
/// faults it — the job never refunded at all.
#[tokio::test]
async fn a_stale_offer_re_matches_to_a_live_node_and_the_buyer_is_served() {
    let identity = LocalIdentity::generate("coordinator@e2e");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    let config = CoordinatorConfig {
        long_poll_timeout: Duration::from_secs(2),
        reoffer_after: Duration::from_millis(150),
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::new(identity, config, reputation, payout.clone(), audit.clone());
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state.clone()).await;
    let http = reqwest::Client::new();

    // The doomed assignee: cheapest ask wins the match, then it never
    // polls. Registration is all it ever does.
    let dead_identity = LocalIdentity::generate("dead-operator@e2e");
    let dead_pubkey = dead_identity.agent_id().pubkey_base58();
    let dead_session = http
        .post(format!("{base_url}/federation/operators/register"))
        .json(
            &RegisterRequest::sign(
                cpu_profile(&dead_identity, 500),
                payout_addr(20),
                &dead_identity,
            )
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

    // The live sibling: pricier, so it loses the first match, but it
    // is the one actually serving.
    let live_identity = LocalIdentity::generate("live-operator@e2e");
    let live_pubkey = live_identity.agent_id().pubkey_base58();
    let live_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        2,
    ));
    let live_profile = cpu_profile(&live_identity, 900);
    live_client
        .register(
            RegisterRequest::sign(live_profile.clone(), payout_addr(19), &live_identity).unwrap(),
        )
        .await
        .unwrap();
    let live_node = Node::new(
        live_identity,
        live_profile,
        live_client,
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

    let buyer = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let envelope = signed_envelope(&buyer, job_id, 1_000, 30_000, epoch_ms(), "e2e-reoffer");
    let submit = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit.status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(
        state.jobs().get(job_id).unwrap().operator_pubkey_b58,
        dead_pubkey,
        "the cheaper ask wins the first match"
    );

    // Let the offer go stale, then drive the sweep the way the
    // periodic task does. The dead assignee is past nothing but the
    // 150ms window — it is still inside the liveness cutoff, so the
    // matcher would happily pick it again if the sweep did not ask for
    // a fresh match over the live directory; the live sibling wins
    // because staleness of the OFFER, not of the operator, is what
    // triggers the move — and the guard prefers whoever the matcher
    // picks now.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let moved = covenant_compute_coordinator::sweep_stale_offers(&state, epoch_ms()).await;
    assert_eq!(moved, vec![job_id]);

    let record = state.jobs().get(job_id).unwrap();
    assert_eq!(record.operator_pubkey_b58, live_pubkey);
    assert_eq!(record.payout_address, payout_addr(19));
    assert_eq!(
        record.phase,
        covenant_compute_coordinator::JobPhase::Offered,
        "money untouched by the move"
    );

    // The live node serves it like any other offer.
    let outcome = live_node
        .run_once()
        .await
        .expect("run_once")
        .expect("the re-offered job reaches the live node");
    assert_eq!(outcome.job_id, job_id);
    assert_eq!(outcome.receipt.receipt.status, A2ATaskStatus::Ok);
    assert_eq!(
        state.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released
    );
    let records = payout.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].payout_address, payout_addr(19));

    // The vanished assignee finally wakes up and tries to accept the
    // job it still thinks is its own. The session check compares it
    // against the job's CURRENT assignee, so a non-assignee's decision
    // is 401 before any write — the mid-request interleaving (record
    // read, then reassignment, then write) is the narrower window the
    // in-handler assignee guard answers 409 for, pinned at the book
    // in `set_phase_if_assigned_refuses_a_reassigned_away_operator`.
    let late_accept = http
        .post(format!("{base_url}/federation/jobs/{job_id}/accept"))
        .bearer_auth(&dead_session)
        .json(&serde_json::json!({ "decision": "accept", "job_id": job_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(late_accept.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(
        state.jobs().get(job_id).unwrap().phase,
        covenant_compute_coordinator::JobPhase::Completed,
        "the late decision changed nothing"
    );

    // Routing history exists; no fault ever attached to the vanished
    // assignee — the job was served, not refunded.
    let events = audit.recent(50).await.unwrap();
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        covenant_audit::AuditKind::ComputeJobReoffered {
            job_id: id,
            from_operator_pubkey_b58: from,
            to_operator_pubkey_b58: to,
        } if *id == job_id && *from == dead_pubkey && *to == live_pubkey
    )));
    assert!(!events.iter().any(|e| matches!(
        &e.kind,
        covenant_audit::AuditKind::ComputeJobRefunded { .. }
    )));
}

#[tokio::test]
async fn an_offline_heartbeat_re_matches_queued_offers_before_any_sweep() {
    use covenant_compute_protocol::{HeartbeatRequest, OperatorStatus};

    let identity = LocalIdentity::generate("coordinator@e2e");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    // Deliberately the default 60s re-offer window: nothing but the
    // Offline declaration itself can move the job inside this test.
    let config = CoordinatorConfig {
        long_poll_timeout: Duration::from_secs(2),
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::new(identity, config, reputation, payout.clone(), audit.clone());
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state.clone()).await;
    let http = reqwest::Client::new();

    // The declarer: cheapest ask wins the match, then its model server
    // dies before it ever polls — exactly what the node's serve-loop
    // gate does when `wait_for_backend` finds the backend down.
    let declarer_identity = LocalIdentity::generate("declarer-operator@e2e");
    let declarer_pubkey = declarer_identity.agent_id().pubkey_base58();
    http.post(format!("{base_url}/federation/operators/register"))
        .json(
            &RegisterRequest::sign(
                cpu_profile(&declarer_identity, 500),
                payout_addr(27),
                &declarer_identity,
            )
            .unwrap(),
        )
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let live_identity = LocalIdentity::generate("live-operator@e2e");
    let live_pubkey = live_identity.agent_id().pubkey_base58();
    let live_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        2,
    ));
    let live_profile = cpu_profile(&live_identity, 900);
    live_client
        .register(
            RegisterRequest::sign(live_profile.clone(), payout_addr(19), &live_identity).unwrap(),
        )
        .await
        .unwrap();
    let live_node = Node::new(
        live_identity,
        live_profile,
        live_client,
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

    let buyer = LocalIdentity::generate("buyer@e2e");
    let job_id = Uuid::new_v4();
    let envelope = signed_envelope(
        &buyer,
        job_id,
        1_000,
        30_000,
        epoch_ms(),
        "e2e-offline-heal",
    );
    let submit = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit.status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(
        state.jobs().get(job_id).unwrap().operator_pubkey_b58,
        declarer_pubkey,
        "the cheaper ask wins the first match"
    );

    // The declarer says the one honest thing a dying node can say. The
    // beat's ack returns only after the heal ran — no sweep, no wait.
    let offline_beat = HeartbeatRequest::sign(
        declarer_identity.agent_id(),
        OperatorStatus::Offline,
        0,
        epoch_ms(),
        &declarer_identity,
    )
    .unwrap();
    http.post(format!("{base_url}/federation/operators/heartbeat"))
        .json(&offline_beat)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let record = state.jobs().get(job_id).unwrap();
    assert_eq!(record.operator_pubkey_b58, live_pubkey);
    assert_eq!(record.payout_address, payout_addr(19));
    assert_eq!(
        record.phase,
        covenant_compute_coordinator::JobPhase::Offered,
        "money untouched by the move"
    );

    // A repeated Offline beat is a no-op: the transition already ran.
    let repeat = HeartbeatRequest::sign(
        declarer_identity.agent_id(),
        OperatorStatus::Offline,
        0,
        epoch_ms(),
        &declarer_identity,
    )
    .unwrap();
    http.post(format!("{base_url}/federation/operators/heartbeat"))
        .json(&repeat)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let outcome = live_node
        .run_once()
        .await
        .expect("run_once")
        .expect("the healed offer reaches the live node");
    assert_eq!(outcome.job_id, job_id);
    assert_eq!(outcome.receipt.receipt.status, A2ATaskStatus::Ok);
    assert_eq!(
        state.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released
    );
    let records = payout.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].payout_address, payout_addr(19));

    // Exactly one move on the record — routing history, not a fault.
    let events = audit.recent(50).await.unwrap();
    let moves: Vec<_> = events
        .iter()
        .filter(|e| matches!(&e.kind, AuditKind::ComputeJobReoffered { .. }))
        .collect();
    assert_eq!(moves.len(), 1);
    assert!(matches!(
        &moves[0].kind,
        AuditKind::ComputeJobReoffered {
            job_id: id,
            from_operator_pubkey_b58: from,
            to_operator_pubkey_b58: to,
        } if *id == job_id && *from == declarer_pubkey && *to == live_pubkey
    ));
    assert!(!events
        .iter()
        .any(|e| matches!(&e.kind, AuditKind::ComputeJobRefunded { .. })));
}

/// splitmix64 — the same deterministic generator as the book models,
/// so a conservation failure names its seed and action index.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Every money total in the system, read across the books in one
/// pass — what the conservation audit returns so a restart can be
/// checked for replaying to the *identical* books, not merely
/// self-consistent ones.
#[derive(Debug, PartialEq)]
struct MoneyTotals {
    deposited: u64,
    available: u64,
    withdrawn: u64,
    held: u64,
    released: u64,
    refunded: u64,
    fees_captured: u64,
    payouts_pushed: u64,
    partner_accrued: std::collections::BTreeMap<String, u64>,
    bond_posted: u64,
    bond_slashed: u64,
    bond_refunded: u64,
    bond_at_stake: u64,
    bond_unbonding: u64,
    backend_payout_total: u64,
    backend_transfer_total: u64,
}

/// The cross-book money-conservation audit: reads every ledger the
/// coordinator keeps — buyer deposits, withdrawals, escrow holds, the
/// job book's fee/share/payout rows, the partner accrual derivation,
/// the bond book and the payout backend — and asserts the whole-system
/// law plus every per-job and per-buyer split, each figure checked
/// against a *different* book than the one that produced it.
async fn audit_money_conservation(
    state: &CoordinatorState,
    payout: &MockPayout,
    buyer_keys: &[String],
    operator_key: &str,
    expected_deposited: u64,
    expected_bond_posted: u64,
    ctx: &str,
) -> MoneyTotals {
    use std::collections::{BTreeMap, HashMap};

    use covenant_compute_coordinator::JobPhase;

    // Escrow walk. The workload is organic-only: no subsidy policy is
    // attached, so a Bootstrap hold appearing here is itself a bug.
    let holds: HashMap<Uuid, covenant_compute_coordinator::EscrowHoldState> =
        state.escrow().holds_snapshot().into_iter().collect();
    let (mut held, mut released, mut refunded) = (0u64, 0u64, 0u64);
    for hold in holds.values() {
        assert_eq!(
            hold.funding_source,
            FundingSource::Organic,
            "{ctx}: only buyer-funded holds exist in this workload"
        );
        match hold.status {
            EscrowStatus::Held => held += hold.amount_micro_usdc,
            EscrowStatus::Released => released += hold.amount_micro_usdc,
            EscrowStatus::Refunded => refunded += hold.amount_micro_usdc,
        }
    }

    // Job walk, via the per-buyer index: every hold must belong to
    // exactly one record and vice versa, whatever the outcome was.
    let mut records: HashMap<Uuid, covenant_compute_coordinator::JobRecord> = HashMap::new();
    for key in buyer_keys {
        for (job_id, record) in state.jobs().by_buyer(key) {
            records.insert(job_id, record);
        }
    }
    assert_eq!(
        records.len(),
        holds.len(),
        "{ctx}: holds and job records must be a bijection on job id"
    );

    let (mut payouts_pushed, mut fees) = (0u64, 0u64);
    let mut shares_by_code: BTreeMap<String, u64> = BTreeMap::new();
    for (job_id, record) in &records {
        let hold = holds
            .get(job_id)
            .unwrap_or_else(|| panic!("{ctx}: job {job_id} has no escrow hold"));
        assert_eq!(
            hold.amount_micro_usdc, record.envelope.payload.price_micro_usdc,
            "{ctx}: job {job_id}: the hold is exactly the buyer-signed price"
        );
        match record.phase {
            JobPhase::Completed => {
                assert_eq!(
                    hold.status,
                    EscrowStatus::Released,
                    "{ctx}: job {job_id}: completed means the hold released"
                );
                let paid = record
                    .payout
                    .as_ref()
                    .unwrap_or_else(|| panic!("{ctx}: job {job_id}: completed but never paid"))
                    .amount_micro_usdc;
                assert_eq!(
                    paid + record.fee_micro_usdc,
                    hold.amount_micro_usdc,
                    "{ctx}: job {job_id}: gross must split exactly into operator net + fee"
                );
                assert!(
                    record.partner_share_micro_usdc + record.buyer_partner_share_micro_usdc
                        <= record.fee_micro_usdc,
                    "{ctx}: job {job_id}: partner shares may never sum past the fee"
                );
                payouts_pushed += paid;
                fees += record.fee_micro_usdc;
                if record.partner_share_micro_usdc > 0 {
                    *shares_by_code
                        .entry(record.referral_code.clone().unwrap_or_else(|| {
                            panic!("{ctx}: job {job_id}: supply share with no code")
                        }))
                        .or_default() += record.partner_share_micro_usdc;
                }
                if record.buyer_partner_share_micro_usdc > 0 {
                    *shares_by_code
                        .entry(record.buyer_referral_code.clone().unwrap_or_else(|| {
                            panic!("{ctx}: job {job_id}: buyer share with no code")
                        }))
                        .or_default() += record.buyer_partner_share_micro_usdc;
                }
            }
            JobPhase::Failed | JobPhase::Refunded => {
                assert_eq!(
                    hold.status,
                    EscrowStatus::Refunded,
                    "{ctx}: job {job_id}: a concluded non-payment must refund the hold"
                );
                assert_eq!(record.fee_micro_usdc, 0, "{ctx}: no fee off a refund");
                assert_eq!(
                    record.partner_share_micro_usdc + record.buyer_partner_share_micro_usdc,
                    0,
                    "{ctx}: no accrual off a refund"
                );
                assert!(
                    record.payout.is_none(),
                    "{ctx}: job {job_id}: a refunded job must never carry a payout"
                );
            }
            JobPhase::Offered | JobPhase::Accepted => {
                assert_eq!(
                    hold.status,
                    EscrowStatus::Held,
                    "{ctx}: job {job_id}: in-flight means the hold is still standing"
                );
                assert!(record.payout.is_none());
            }
            other => panic!("{ctx}: job {job_id}: workload never drives phase {other:?}"),
        }
    }

    // The job book's own aggregates must agree with the walk.
    let (fees_captured, _) = state.jobs().fees_captured();
    assert_eq!(
        fees_captured, fees,
        "{ctx}: fees_captured drifted from the per-job rows"
    );
    let accruals = state.jobs().partner_accruals();
    let accrued_by_code: BTreeMap<String, u64> = accruals
        .iter()
        .map(|(code, (amount, _))| (code.clone(), *amount))
        .collect();
    assert_eq!(
        accrued_by_code, shares_by_code,
        "{ctx}: partner accruals drifted from the per-job share rows"
    );
    let partner_accrued_total: u64 = shares_by_code.values().sum();
    assert!(
        partner_accrued_total <= fees_captured,
        "{ctx}: partners may never accrue more than the fees that fund them"
    );
    for code in shares_by_code.keys() {
        assert!(
            state.partner_payouts().paid(code) <= shares_by_code[code],
            "{ctx}: partner {code} paid past its accrual"
        );
    }

    // The payout backend agrees with the job book, job by job: every
    // completed job was pushed exactly once, and nothing else ever was.
    let mut backend_by_job: HashMap<Uuid, u64> = HashMap::new();
    for record in payout.records() {
        assert!(
            backend_by_job
                .insert(record.job_id, record.amount_micro_usdc)
                .is_none(),
            "{ctx}: job {} paid twice by the backend",
            record.job_id
        );
    }
    let completed: Vec<_> = records
        .iter()
        .filter(|(_, r)| r.phase == JobPhase::Completed)
        .collect();
    assert_eq!(
        backend_by_job.len(),
        completed.len(),
        "{ctx}: backend payouts and completed jobs must be a bijection"
    );
    for (job_id, record) in &completed {
        assert_eq!(
            backend_by_job.get(*job_id).copied(),
            record.payout.as_ref().map(|p| p.amount_micro_usdc),
            "{ctx}: job {job_id}: the backend and the job book disagree on the push"
        );
    }
    let backend_payout_total: u64 = backend_by_job.values().sum();

    // Buyer books, buyer by buyer and in total.
    let (mut deposited, mut available, mut withdrawn) = (0u64, 0u64, 0u64);
    for key in buyer_keys {
        let funds = state.buyer_funds(key);
        assert_eq!(
            funds.deposited_micro_usdc,
            funds.available_micro_usdc + funds.withdrawn_micro_usdc + funds.charged_micro_usdc,
            "{ctx}: buyer {key}: deposits split exactly into available + withdrawn + charged"
        );
        let charged_from_escrow: u64 = holds
            .values()
            .filter(|h| h.buyer_pubkey_b58 == *key && h.status != EscrowStatus::Refunded)
            .map(|h| h.amount_micro_usdc)
            .sum();
        assert_eq!(
            funds.charged_micro_usdc, charged_from_escrow,
            "{ctx}: buyer {key}: the funds view drifted from the escrow walk"
        );
        deposited += funds.deposited_micro_usdc;
        available += funds.available_micro_usdc;
        withdrawn += funds.withdrawn_micro_usdc;
    }
    assert_eq!(
        deposited, expected_deposited,
        "{ctx}: the deposit book drifted from what the rail verifiably credited"
    );

    // THE LAW. Every micro-USDC a buyer ever put in is exactly one of:
    // still spendable, withdrawn back out, locked behind an in-flight
    // job, or spent on a released one — and every released coin is
    // exactly operator net or captured fee, nothing minted, nothing
    // burned. Each term comes from a different book.
    assert_eq!(
        deposited,
        available + withdrawn + held + released,
        "{ctx}: conservation violated across accounts/withdrawals/escrow"
    );
    assert_eq!(
        released,
        payouts_pushed + fees_captured,
        "{ctx}: released gross must equal operator payouts plus captured fees"
    );

    // Every withdrawal debit was honored by exactly one backend
    // transfer; matured unbond refunds account for the rest, and no
    // other transfer ever left the backend.
    let transfers: HashMap<Uuid, u64> = payout
        .transfers()
        .into_iter()
        .map(|t| (t.transfer_id, t.amount_micro_usdc))
        .collect();
    let mut withdrawal_count = 0usize;
    for key in buyer_keys {
        for w in state.withdrawals().for_buyer(key) {
            assert!(
                w.pushed.is_some(),
                "{ctx}: withdrawal {} debited but never pushed",
                w.withdrawal_id
            );
            assert_eq!(
                transfers.get(&w.withdrawal_id).copied(),
                Some(w.amount_micro_usdc),
                "{ctx}: withdrawal {} has no matching backend transfer",
                w.withdrawal_id
            );
            withdrawal_count += 1;
        }
    }
    let bonds = state.bonds().status(operator_key);
    let mut unbond_pushed_count = 0usize;
    let mut unbond_paid_total = 0u64;
    for unbond in state.bonds().unbonds_for(operator_key) {
        if let Some(push) = &unbond.pushed {
            unbond_paid_total += push.paid_micro_usdc;
            if push.paid_micro_usdc > 0 {
                assert_eq!(
                    transfers.get(&unbond.unbond_id).copied(),
                    Some(push.paid_micro_usdc),
                    "{ctx}: unbond {} has no matching backend transfer",
                    unbond.unbond_id
                );
                unbond_pushed_count += 1;
            }
        }
    }
    assert_eq!(
        transfers.len(),
        withdrawal_count + unbond_pushed_count,
        "{ctx}: a transfer left the backend that no book asked for"
    );

    // Bond conservation, through at_stake (committed saturates when a
    // slash lands mid-maturation; at_stake never does — slashes clamp
    // at the standing stake by construction).
    assert_eq!(
        bonds.posted_micro_usdc, expected_bond_posted,
        "{ctx}: the bond book drifted from what the rail verifiably credited"
    );
    assert_eq!(
        bonds.posted_micro_usdc,
        bonds.slashed_micro_usdc + bonds.refunded_micro_usdc + bonds.at_stake_micro_usdc,
        "{ctx}: bond conservation violated"
    );
    assert_eq!(
        bonds.refunded_micro_usdc, unbond_paid_total,
        "{ctx}: the bond status drifted from the unbond push rows"
    );
    let slashed_from_rows: u64 = state
        .bonds()
        .slashes_for(operator_key)
        .iter()
        .map(|s| s.amount_micro_usdc)
        .sum();
    assert_eq!(
        bonds.slashed_micro_usdc, slashed_from_rows,
        "{ctx}: the bond status drifted from the slash rows"
    );

    MoneyTotals {
        deposited,
        available,
        withdrawn,
        held,
        released,
        refunded,
        fees_captured,
        payouts_pushed,
        partner_accrued: shares_by_code,
        bond_posted: bonds.posted_micro_usdc,
        bond_slashed: bonds.slashed_micro_usdc,
        bond_refunded: bonds.refunded_micro_usdc,
        bond_at_stake: bonds.at_stake_micro_usdc,
        bond_unbonding: bonds.unbonding_micro_usdc,
        backend_payout_total,
        backend_transfer_total: transfers.values().sum(),
    }
}

/// Preloads a rail-verified deposit and claims it through the real
/// endpoint — the only way money enters the buyer books.
async fn credit_deposit_via_rail(
    rail: &covenant_compute_coordinator::MockRail,
    http: &reqwest::Client,
    config: &covenant_compute_buyer::BuyerConfig,
    buyer: &LocalIdentity,
    amount_micro_usdc: u64,
    deposit_id: String,
) {
    rail.preload(covenant_compute_coordinator::VerifiedDeposit {
        deposit_id: deposit_id.clone(),
        buyer_pubkey_b58: buyer.agent_id().pubkey_base58(),
        amount_micro_usdc,
    });
    covenant_compute_buyer::claim_deposit(http, config, buyer, &deposit_id)
        .await
        .unwrap();
}

/// Posts the assigned operator's signed result for a matched job — an
/// `Ok` receipt that releases, or the operator's own signed failure
/// that refunds.
async fn post_signed_result(
    http: &reqwest::Client,
    base_url: &str,
    operator: &LocalIdentity,
    job_id: Uuid,
    price_micro_usdc: u64,
    ok: bool,
) {
    let output = vec![Content::text(if ok {
        "conserved"
    } else {
        "backend gave out"
    })];
    let receipt = SignedWorkReceipt::sign(
        WorkReceiptPayload {
            job_id,
            operator: operator.agent_id(),
            job_hash_hex: "aa".repeat(32),
            result_hash_hex: covenant_compute_protocol::output_hash_hex(&output),
            meter: JobMeter {
                wall_ms: 5,
                tokens_in: None,
                tokens_out: None,
                gpu_seconds: None,
                finish_reason: None,
            },
            price_micro_usdc,
            status: if ok {
                A2ATaskStatus::Ok
            } else {
                A2ATaskStatus::Error
            },
            executed_at_ms: epoch_ms(),
            node_audit_root_hex: "cc".repeat(32),
        },
        operator,
    )
    .unwrap();
    let resp = http
        .post(format!("{base_url}/federation/jobs/{job_id}/result"))
        .json(&covenant_compute_protocol::JobResultMessage { receipt, output })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
}

/// The whole-system money-conservation law (the one assertion no
/// per-book model states): drive a seeded random multi-outcome
/// workload — deposits, jobs that complete, fail, find no operator,
/// expire or stay in flight, withdrawals, bond posts, slashes, unbond
/// exits — through the real HTTP stack, then audit every ledger
/// against every other. Deterministic per seed; each seed then
/// restarts the coordinator from its journal and requires the replayed
/// books to be *identical*, not merely self-consistent.
#[tokio::test]
async fn money_is_conserved_across_all_books_under_a_random_multi_outcome_workload() {
    use covenant_compute_coordinator::{
        sweep::sweep_matured_unbonds, MockRail, PartnerConfig, VerifiedBond,
    };
    use covenant_compute_protocol::UnbondRequest;

    for seed in 0..4u64 {
        let mut rng = Rng::new(0xC0_1517 ^ (seed << 16));
        let dir = tempfile::tempdir().unwrap();
        let identity_path = dir.path().join("identity.json");
        let journal_path = dir.path().join("journal.jsonl");
        let rail = Arc::new(MockRail::new());
        let payout = Arc::new(MockPayout::new());

        let make_state = |rail: Arc<MockRail>, payout: Arc<MockPayout>| {
            let identity =
                LocalIdentity::load_or_create(&identity_path, "coordinator@conserve").unwrap();
            let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
            let journal_path = journal_path.clone();
            async move {
                // Both partners at 60% of a 20% fee, so the remainder
                // clamp bites on every shared job the workload deals.
                let mut partners = std::collections::HashMap::new();
                partners.insert(
                    "partner-s".to_string(),
                    PartnerConfig::new("partner-s-address".into(), 6_000).unwrap(),
                );
                partners.insert(
                    "partner-b".to_string(),
                    PartnerConfig::new("partner-b-address".into(), 6_000).unwrap(),
                );
                CoordinatorState::with_journal(
                    identity,
                    CoordinatorConfig {
                        long_poll_timeout: Duration::from_secs(2),
                        default_funding_source: FundingSource::Organic,
                        require_prefunded_buyers: true,
                        fee: covenant_compute_protocol::MarketplaceFee::new(2_000).unwrap(),
                        partners,
                        unbond_window: Duration::ZERO,
                        ..CoordinatorConfig::default()
                    },
                    Arc::new(AuditReputationSource::new(audit.clone())),
                    payout,
                    audit,
                    &journal_path,
                    Some(rail),
                )
                .await
                .unwrap()
            }
        };

        let state = make_state(rail.clone(), payout.clone()).await;
        let (base_url, server) = spawn_coordinator_abortable(state.clone()).await;
        let http = reqwest::Client::new();

        // One operator asking 1 (every priced job matches), registered
        // with a supply-side attribution; results are posted directly,
        // the complete_one_job pattern, so outcomes stay scripted.
        let operator = LocalIdentity::generate("operator@conserve");
        let operator_key = operator.agent_id().pubkey_base58();
        let coordinator_client =
            HttpCoordinatorClient::with_config(base_url.clone(), Duration::from_secs(2), 1);
        coordinator_client
            .register(
                RegisterRequest::sign_referred(
                    cpu_profile(&operator, 1),
                    payout_addr(6),
                    Some("partner-s".into()),
                    &operator,
                )
                .unwrap(),
            )
            .await
            .unwrap();

        let buyers: Vec<LocalIdentity> = (0..3)
            .map(|i| LocalIdentity::generate(format!("buyer-{i}@conserve")))
            .collect();
        let buyer_keys: Vec<String> = buyers
            .iter()
            .map(|b| b.agent_id().pubkey_base58())
            .collect();
        let buyer_config = covenant_compute_buyer::BuyerConfig {
            coordinator_url: base_url.clone(),
            poll_interval: Duration::from_millis(50),
            referral_code: None,
            rpc_url: None,
        };
        let wallet = LocalIdentity::generate("wallet@conserve")
            .agent_id()
            .pubkey_base58();

        // Ground truths the audit checks the books against: what the
        // rail verifiably credited, tracked outside every ledger.
        let mut deposited_expected = 0u64;
        let mut bond_posted_expected = 0u64;

        for (i, buyer) in buyers.iter().enumerate() {
            let amount = 2_000 + rng.below(4_001);
            credit_deposit_via_rail(
                &rail,
                &http,
                &buyer_config,
                buyer,
                amount,
                format!("dep-{seed}-init-{i}"),
            )
            .await;
            deposited_expected += amount;
        }

        for action in 0..32u64 {
            let ctx = format!("seed {seed} action {action}");
            match rng.below(100) {
                // A verified deposit lands.
                0..=17 => {
                    let buyer = rng.below(3) as usize;
                    let amount = 500 + rng.below(4_501);
                    credit_deposit_via_rail(
                        &rail,
                        &http,
                        &buyer_config,
                        &buyers[buyer],
                        amount,
                        format!("dep-{seed}-{action}"),
                    )
                    .await;
                    deposited_expected += amount;
                }
                // A priced job: funded submits match and then complete,
                // fail, or deliberately stay in flight; an unfunded one
                // is refused with nothing moved.
                18..=55 => {
                    let buyer = &buyers[rng.below(3) as usize];
                    let price = 1 + rng.below(3_000);
                    let referred = rng.below(2) == 0;
                    let outcome = rng.below(10);
                    let job_id = Uuid::new_v4();
                    let idem = format!("conserve-{seed}-{action}");
                    let envelope = if referred {
                        referred_envelope(buyer, job_id, price, &idem, "partner-b")
                    } else {
                        signed_envelope(buyer, job_id, price, 30_000, epoch_ms(), &idem)
                    };
                    let resp = http
                        .post(format!("{base_url}/federation/jobs"))
                        .json(&envelope)
                        .send()
                        .await
                        .unwrap();
                    if resp.status() == reqwest::StatusCode::PAYMENT_REQUIRED {
                        continue;
                    }
                    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED, "{ctx}");
                    match outcome {
                        0..=6 => {
                            post_signed_result(&http, &base_url, &operator, job_id, price, true)
                                .await
                        }
                        7..=8 => {
                            post_signed_result(&http, &base_url, &operator, job_id, price, false)
                                .await
                        }
                        _ => {} // stays in flight, escrow held
                    }
                }
                // A job no registered operator can serve: admission
                // fails, the fresh hold refunds immediately.
                56..=61 => {
                    let buyer = &buyers[rng.below(3) as usize];
                    let payload = JobEnvelopePayload {
                        job_id: Uuid::new_v4(),
                        buyer: buyer.agent_id(),
                        kind: JobKind::InferenceCall,
                        capability_requirement: CapabilityRequirement {
                            gpu_class: None,
                            min_vram_gb: None,
                            model_id: None,
                            kind: JobKind::InferenceCall,
                            max_duration_secs: 30,
                            min_reputation_bps: None,
                        },
                        input: vec![Content::text("infer: nobody serves this")],
                        price_micro_usdc: 1 + rng.below(1_000),
                        deadline_ms: 30_000,
                        idempotency: A2AIdempotency::new(
                            A2ADuplicateSafety::Idempotent,
                            format!("conserve-{seed}-{action}"),
                        ),
                        issued_at_ms: epoch_ms(),
                        referral_code: None,
                        stream: false,
                    };
                    let envelope = SignedJobEnvelope::sign(payload, buyer).unwrap();
                    let resp = http
                        .post(format!("{base_url}/federation/jobs"))
                        .json(&envelope)
                        .send()
                        .await
                        .unwrap();
                    assert!(
                        resp.status() == reqwest::StatusCode::CONFLICT
                            || resp.status() == reqwest::StatusCode::PAYMENT_REQUIRED,
                        "{ctx}: unmatched submit came back {}",
                        resp.status()
                    );
                }
                // A withdrawal: honored when covered, refused whole
                // when not — either way the books stay balanced.
                62..=76 => {
                    let buyer = rng.below(3) as usize;
                    let amount = 1 + rng.below(4_000);
                    match covenant_compute_buyer::withdraw(
                        &http,
                        &buyer_config,
                        &buyers[buyer],
                        Uuid::new_v4(),
                        amount,
                        &wallet,
                    )
                    .await
                    {
                        Ok(view) => assert!(view.pushed, "{ctx}: mock backend pushes inline"),
                        Err(e) => assert!(
                            e.to_string().contains("402"),
                            "{ctx}: only insufficient funds may refuse a withdrawal: {e}"
                        ),
                    }
                }
                // The operator posts (more) stake.
                77..=85 => {
                    let amount = 10_000 + rng.below(40_001);
                    let bond_id = format!("bond-{seed}-{action}");
                    rail.preload_bond(VerifiedBond {
                        bond_id: bond_id.clone(),
                        operator_pubkey_b58: operator_key.clone(),
                        amount_micro_usdc: amount,
                    });
                    let resp = http
                        .post(format!("{base_url}/federation/operators/bond"))
                        .json(&serde_json::json!({
                            "operator_pubkey_b58": operator_key,
                            "bond_id": bond_id,
                        }))
                        .send()
                        .await
                        .unwrap();
                    assert!(resp.status().is_success(), "{ctx}");
                    bond_posted_expected += amount;
                }
                // A coordinator-proven fault takes stake (a no-op while
                // none is standing — that path is part of the model).
                86..=92 => {
                    state
                        .slash_for_fault(
                            "canary",
                            Uuid::new_v4(),
                            &operator_key,
                            1 + rng.below(2_000),
                            "conservation drill",
                        )
                        .await;
                }
                // The operator draws down committed stake.
                93..=96 => {
                    let committed = state.bonds().status(&operator_key).committed_micro_usdc;
                    if committed == 0 {
                        continue;
                    }
                    let amount = 1 + rng.below(committed);
                    let unbond = UnbondRequest::sign(
                        operator.agent_id(),
                        Uuid::new_v4(),
                        amount,
                        wallet.clone(),
                        epoch_ms(),
                        &operator,
                    )
                    .unwrap();
                    let resp = http
                        .post(format!("{base_url}/federation/operators/unbond"))
                        .json(&unbond)
                        .send()
                        .await
                        .unwrap();
                    assert!(resp.status().is_success(), "{ctx}");
                }
                // The maturation sweep pushes whatever matured refunds
                // are owed (the zero window matures them instantly).
                _ => {
                    sweep_matured_unbonds(&state).await;
                }
            }
        }

        // One funded job runs out its deadline unserved: the sweep
        // refunds it the way a deployment's periodic tick would.
        credit_deposit_via_rail(
            &rail,
            &http,
            &buyer_config,
            &buyers[0],
            1_000,
            format!("dep-{seed}-expiry"),
        )
        .await;
        deposited_expected += 1_000;
        let expiry_job = Uuid::new_v4();
        let issued_at_ms = epoch_ms();
        let deadline_ms = 1_000;
        let resp = http
            .post(format!("{base_url}/federation/jobs"))
            .json(&signed_envelope(
                &buyers[0],
                expiry_job,
                500,
                deadline_ms,
                issued_at_ms,
                &format!("conserve-{seed}-expiry"),
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
        // The sweep's clock, not the wall clock, decides expiry — the
        // same synthetic-now pattern as the dedicated deadline test.
        let refunded = sweep_expired(&state, issued_at_ms + deadline_ms + 1).await;
        assert!(
            refunded.contains(&expiry_job),
            "seed {seed}: the deadline sweep must refund the unserved job"
        );

        let before = audit_money_conservation(
            &state,
            &payout,
            &buyer_keys,
            &operator_key,
            deposited_expected,
            bond_posted_expected,
            &format!("seed {seed} before restart"),
        )
        .await;

        // The law must be scrapable, not just book-derivable: every
        // aggregate the audit read is on /metrics, and the conservation
        // identities hold on the scraped numbers alone — what a
        // deployment's alerting watches for books drift.
        let scrape = http
            .get(format!("{base_url}/metrics"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let metric = |name: &str| -> u64 {
            scrape
                .lines()
                .find_map(|l| l.strip_prefix(name).and_then(|rest| rest.strip_prefix(' ')))
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| panic!("seed {seed}: metric {name} missing from scrape"))
        };
        for (name, book_value) in [
            ("compute_deposited_micro_usdc_total", before.deposited),
            ("compute_withdrawn_micro_usdc_total", before.withdrawn),
            ("compute_buyer_available_micro_usdc", before.available),
            (
                "compute_escrow_held_micro_usdc{funding_source=\"organic\"}",
                before.held,
            ),
            (
                "compute_escrow_released_micro_usdc_total{funding_source=\"organic\"}",
                before.released,
            ),
            (
                "compute_escrow_refunded_micro_usdc_total{funding_source=\"organic\"}",
                before.refunded,
            ),
            (
                "compute_payouts_pushed_micro_usdc_total",
                before.payouts_pushed,
            ),
            ("compute_payouts_outstanding_micro_usdc", 0),
            (
                "compute_fees_captured_micro_usdc_total",
                before.fees_captured,
            ),
            (
                "compute_partner_accrued_micro_usdc_total",
                before.partner_accrued.values().sum(),
            ),
            ("compute_partner_paid_micro_usdc_total", 0),
            ("compute_bonds_posted_micro_usdc_total", before.bond_posted),
            (
                "compute_bonds_slashed_micro_usdc_total",
                before.bond_slashed,
            ),
            (
                "compute_bonds_refunded_micro_usdc_total",
                before.bond_refunded,
            ),
            ("compute_bonds_at_stake_micro_usdc", before.bond_at_stake),
            ("compute_bonds_unbonding_micro_usdc", before.bond_unbonding),
        ] {
            assert_eq!(
                metric(name),
                book_value,
                "seed {seed}: scraped {name} drifted from the books"
            );
        }
        assert_eq!(
            metric("compute_deposited_micro_usdc_total"),
            metric("compute_buyer_available_micro_usdc")
                + metric("compute_withdrawn_micro_usdc_total")
                + metric("compute_escrow_held_micro_usdc{funding_source=\"organic\"}")
                + metric("compute_escrow_released_micro_usdc_total{funding_source=\"organic\"}"),
            "seed {seed}: the scraped conservation identity must balance"
        );
        assert_eq!(
            metric("compute_escrow_released_micro_usdc_total{funding_source=\"organic\"}")
                + metric("compute_escrow_released_micro_usdc_total{funding_source=\"bootstrap\"}"),
            metric("compute_payouts_pushed_micro_usdc_total")
                + metric("compute_payouts_outstanding_micro_usdc")
                + metric("compute_fees_captured_micro_usdc_total"),
            "seed {seed}: the scraped released-gross identity must balance"
        );
        assert_eq!(
            metric("compute_bonds_posted_micro_usdc_total"),
            metric("compute_bonds_slashed_micro_usdc_total")
                + metric("compute_bonds_refunded_micro_usdc_total")
                + metric("compute_bonds_at_stake_micro_usdc"),
            "seed {seed}: the scraped bond identity must balance"
        );

        // The coordinator dies and replays its journal. The books must
        // come back identical — same law, same totals, to the coin.
        server.abort();
        drop(state);
        let state = make_state(rail.clone(), payout.clone()).await;
        let after = audit_money_conservation(
            &state,
            &payout,
            &buyer_keys,
            &operator_key,
            deposited_expected,
            bond_posted_expected,
            &format!("seed {seed} after restart"),
        )
        .await;
        assert_eq!(
            before, after,
            "seed {seed}: journal replay must rebuild the money books exactly"
        );
    }
}

/// The sequential workload proves the law; this proves the
/// arbitration. Tasks race deposits, job submissions, settlements and
/// withdrawals over ONE buyer's balance through the real HTTP stack —
/// the exact contention the escrow's holds lock exists to arbitrate
/// (a concurrent hold and withdrawal must never both spend the same
/// deposit). Whatever interleaving the scheduler produces, the books
/// must balance at quiescence and replay identically.
#[tokio::test]
async fn money_stays_conserved_when_a_buyers_books_are_raced() {
    use covenant_compute_coordinator::MockRail;

    let dir = tempfile::tempdir().unwrap();
    let identity_path = dir.path().join("identity.json");
    let journal_path = dir.path().join("journal.jsonl");
    let rail = Arc::new(MockRail::new());
    let payout = Arc::new(MockPayout::new());

    let make_state = |rail: Arc<MockRail>, payout: Arc<MockPayout>| {
        let identity = LocalIdentity::load_or_create(&identity_path, "coordinator@race").unwrap();
        let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
        let journal_path = journal_path.clone();
        async move {
            CoordinatorState::with_journal(
                identity,
                CoordinatorConfig {
                    long_poll_timeout: Duration::from_secs(2),
                    default_funding_source: FundingSource::Organic,
                    require_prefunded_buyers: true,
                    fee: covenant_compute_protocol::MarketplaceFee::new(2_000).unwrap(),
                    ..CoordinatorConfig::default()
                },
                Arc::new(AuditReputationSource::new(audit.clone())),
                payout,
                audit,
                &journal_path,
                Some(rail),
            )
            .await
            .unwrap()
        }
    };

    let state = make_state(rail.clone(), payout.clone()).await;
    let (base_url, server) = spawn_coordinator_abortable(state.clone()).await;
    let http = reqwest::Client::new();

    let operator = Arc::new(LocalIdentity::generate("operator@race"));
    let operator_key = operator.agent_id().pubkey_base58();
    let coordinator_client =
        HttpCoordinatorClient::with_config(base_url.clone(), Duration::from_secs(2), 1);
    coordinator_client
        .register(
            RegisterRequest::sign(cpu_profile(&operator, 1), payout_addr(5), &operator).unwrap(),
        )
        .await
        .unwrap();

    let buyer = Arc::new(LocalIdentity::generate("buyer@race"));
    let buyer_key = buyer.agent_id().pubkey_base58();
    let buyer_config = covenant_compute_buyer::BuyerConfig {
        coordinator_url: base_url.clone(),
        poll_interval: Duration::from_millis(50),
        referral_code: None,
        rpc_url: None,
    };
    let wallet = LocalIdentity::generate("wallet@race")
        .agent_id()
        .pubkey_base58();

    credit_deposit_via_rail(
        &rail,
        &http,
        &buyer_config,
        &buyer,
        10_000,
        "race-seed".into(),
    )
    .await;
    let mut deposited_expected = 10_000u64;

    // Eight tasks, six actions each, all contending for the same
    // deposits. Each returns what it verifiably paid in.
    let mut tasks = Vec::new();
    for task in 0..8u64 {
        let rail = rail.clone();
        let http = http.clone();
        let base_url = base_url.clone();
        let buyer = buyer.clone();
        let buyer_config = buyer_config.clone();
        let operator = operator.clone();
        let wallet = wallet.clone();
        tasks.push(tokio::spawn(async move {
            let mut rng = Rng::new(0x0DDBA11 ^ (task << 24));
            let mut credited = 0u64;
            for action in 0..6u64 {
                match rng.below(10) {
                    0..=2 => {
                        let amount = 300 + rng.below(800);
                        credit_deposit_via_rail(
                            &rail,
                            &http,
                            &buyer_config,
                            &buyer,
                            amount,
                            format!("race-dep-{task}-{action}"),
                        )
                        .await;
                        credited += amount;
                    }
                    3..=6 => {
                        let price = 1 + rng.below(1_500);
                        let outcome = rng.below(10);
                        let job_id = Uuid::new_v4();
                        let resp = http
                            .post(format!("{base_url}/federation/jobs"))
                            .json(&signed_envelope(
                                &buyer,
                                job_id,
                                price,
                                30_000,
                                epoch_ms(),
                                &format!("race-{task}-{action}"),
                            ))
                            .send()
                            .await
                            .unwrap();
                        match resp.status() {
                            reqwest::StatusCode::PAYMENT_REQUIRED => {}
                            reqwest::StatusCode::ACCEPTED => match outcome {
                                0..=6 => {
                                    post_signed_result(
                                        &http, &base_url, &operator, job_id, price, true,
                                    )
                                    .await
                                }
                                7..=8 => {
                                    post_signed_result(
                                        &http, &base_url, &operator, job_id, price, false,
                                    )
                                    .await
                                }
                                _ => {} // stays in flight
                            },
                            other => {
                                panic!("task {task} action {action}: submit came back {other}")
                            }
                        }
                    }
                    _ => {
                        let amount = 1 + rng.below(1_500);
                        match covenant_compute_buyer::withdraw(
                            &http,
                            &buyer_config,
                            &buyer,
                            Uuid::new_v4(),
                            amount,
                            &wallet,
                        )
                        .await
                        {
                            Ok(view) => assert!(view.pushed),
                            Err(e) => assert!(
                                e.to_string().contains("402"),
                                "task {task}: only insufficient funds may refuse: {e}"
                            ),
                        }
                    }
                }
            }
            credited
        }));
    }
    for task in tasks {
        deposited_expected += task.await.unwrap();
    }

    let buyer_keys = [buyer_key];
    let before = audit_money_conservation(
        &state,
        &payout,
        &buyer_keys,
        &operator_key,
        deposited_expected,
        0,
        "race before restart",
    )
    .await;

    server.abort();
    drop(state);
    let state = make_state(rail.clone(), payout.clone()).await;
    let after = audit_money_conservation(
        &state,
        &payout,
        &buyer_keys,
        &operator_key,
        deposited_expected,
        0,
        "race after restart",
    )
    .await;
    assert_eq!(
        before, after,
        "journal replay must rebuild the raced books exactly"
    );
}

/// One normalized fact per feed row, so two fetches of the operator
/// books — before and after a coordinator restart — can be compared
/// for identity, not just self-consistency.
#[derive(Debug, PartialEq)]
struct FeedFact {
    job_id: Uuid,
    status: String,
    price: u64,
    fee: u64,
    net: u64,
    payout: Option<(u64, Option<String>, u64)>,
}

fn feed_snapshot(rows: &[covenant_compute_node::OperatorJobRow]) -> Vec<FeedFact> {
    let mut facts: Vec<FeedFact> = rows
        .iter()
        .map(|r| FeedFact {
            job_id: r.job_id,
            status: r.status.clone(),
            price: r.price_micro_usdc,
            fee: r.fee_micro_usdc,
            net: r.net_micro_usdc,
            payout: r.payout.as_ref().map(|p| {
                (
                    p.amount_micro_usdc,
                    p.tx_signature.clone(),
                    p.recorded_at_ms,
                )
            }),
        })
        .collect();
    facts.sort_by_key(|f| f.job_id);
    facts
}

/// The cross-wire earnings mirror law. The node's private earnings
/// book and the coordinator's signed operator feed are two records of
/// the same money kept by different parties on different machines;
/// this asserts they agree exactly:
///
/// - feed rows that completed ↔ node credit entries, a bijection on
///   job id — a job that refunded, failed or expired appears on no
///   node book and owes net 0 on the feed;
/// - per job, the net the node credited from its registered fee rate
///   equals the net the coordinator disclosed, gross splits exactly
///   into net + fee, and the funding source matches the escrow hold;
/// - a node entry is `Paid` exactly when the feed row carries a payout
///   block, with the same signature and timestamp;
/// - in aggregate, what the node still expects equals the feed's
///   unpushed net equals the job book's outstanding total.
async fn assert_wire_mirror(
    rows: &[covenant_compute_node::OperatorJobRow],
    entries: &[covenant_compute_node::EarningsEntry],
    state: &CoordinatorState,
    ctx: &str,
) {
    use covenant_compute_node::EarningsStatus;

    let completed: std::collections::HashMap<Uuid, &covenant_compute_node::OperatorJobRow> = rows
        .iter()
        .filter(|r| r.status == "completed")
        .map(|r| (r.job_id, r))
        .collect();
    assert_eq!(
        completed.len(),
        entries.len(),
        "{ctx}: every completed feed row must have exactly one node credit entry"
    );
    for entry in entries {
        let row = completed.get(&entry.job_id).unwrap_or_else(|| {
            panic!(
                "{ctx}: the node credited job {} but the feed does not show it completed",
                entry.job_id
            )
        });
        assert_eq!(
            entry.amount_micro_usdc, row.net_micro_usdc,
            "{ctx}: job {}: the net the node credited from its registered fee rate must equal \
             the net the coordinator disclosed",
            entry.job_id
        );
        assert_eq!(
            entry.fee_micro_usdc, row.fee_micro_usdc,
            "{ctx}: job {}: the disclosed fee drifted between the books",
            entry.job_id
        );
        assert_eq!(
            row.net_micro_usdc + row.fee_micro_usdc,
            row.price_micro_usdc,
            "{ctx}: job {}: gross must split exactly into net + fee",
            entry.job_id
        );
        let record = state
            .jobs()
            .get(entry.job_id)
            .expect("a feed row must have a job book record");
        assert_eq!(
            entry.funding_source, record.escrow_hold.funding_source,
            "{ctx}: job {}: funding source drifted between the books",
            entry.job_id
        );
        match (entry.status, &row.payout) {
            (EarningsStatus::Paid, Some(payout)) => {
                assert_eq!(
                    payout.amount_micro_usdc, entry.amount_micro_usdc,
                    "{ctx}: job {}: the pushed amount must be the credited net",
                    entry.job_id
                );
                assert_eq!(
                    entry.paid_tx_signature, payout.tx_signature,
                    "{ctx}: job {}: the node pinned a different payout signature than the \
                     coordinator reported",
                    entry.job_id
                );
                assert_eq!(
                    entry.paid_at_ms,
                    Some(payout.recorded_at_ms),
                    "{ctx}: job {}: the node pinned a different payout timestamp than the \
                     coordinator reported",
                    entry.job_id
                );
            }
            (EarningsStatus::Unpaid, None) => {}
            (status, payout) => panic!(
                "{ctx}: job {}: the node book says {status:?} but the coordinator books say \
                 payout={}",
                entry.job_id,
                payout.is_some()
            ),
        }
    }
    for row in rows.iter().filter(|r| r.status != "completed") {
        assert_eq!(
            row.net_micro_usdc, 0,
            "{ctx}: job {} did not complete; it owes the operator nothing",
            row.job_id
        );
        assert!(
            row.payout.is_none(),
            "{ctx}: job {} did not complete; nothing may have been pushed for it",
            row.job_id
        );
    }
    let ledger_unpaid: u64 = entries
        .iter()
        .filter(|e| e.status == EarningsStatus::Unpaid)
        .map(|e| e.amount_micro_usdc)
        .sum();
    let feed_unpaid: u64 = completed
        .values()
        .filter(|r| r.payout.is_none())
        .map(|r| r.net_micro_usdc)
        .sum();
    let (_pushed, outstanding) = state.jobs().payout_totals();
    assert_eq!(
        ledger_unpaid, feed_unpaid,
        "{ctx}: what the node still expects must equal what the feed shows unpushed"
    );
    assert_eq!(
        feed_unpaid, outstanding,
        "{ctx}: the feed's unpushed net must equal the job book's outstanding total"
    );
}

/// The earnings mirror held to a mixed workload over the real wire —
/// a paid job, a released job whose payout push failed, an executor
/// failure, a deadline expiry — then the payout rail heals, and then
/// BOTH parties restart: the coordinator replays its journal, the node
/// reopens its JSONL ledger, and the books must come back *identical*,
/// with the real reconcile pass finding nothing left to flip.
#[tokio::test]
async fn node_earnings_and_the_operator_feed_stay_one_record_across_the_wire_and_restarts() {
    use std::sync::atomic::{AtomicBool, Ordering};

    use covenant_compute_buyer::{dispatch_and_verify, BuyerConfig, BuyerError, JobRequest};
    use covenant_compute_coordinator::{
        payout::TransferRecord, MockRail, Payout, PayoutError, PayoutRecord,
    };
    use covenant_compute_node::{
        reconcile_paid_rows, EarningsStatus, ExecutionOutcome, ExecutorError, JobExecutor,
        JsonlEarningsLedger,
    };

    // A payout backend with a kill-switch: `pay` refuses while gated,
    // so a released job books as completed-but-outstanding — exactly
    // what a sidecar outage produces.
    struct GateablePayout {
        inner: MockPayout,
        down: AtomicBool,
    }

    #[async_trait::async_trait]
    impl Payout for GateablePayout {
        async fn pay(
            &self,
            job_id: Uuid,
            operator_pubkey_b58: &str,
            payout_address: &str,
            amount_micro_usdc: u64,
            receipt: &SignedWorkReceipt,
        ) -> Result<PayoutRecord, PayoutError> {
            if self.down.load(Ordering::SeqCst) {
                return Err(PayoutError::Backend("payout rail gated off".into()));
            }
            self.inner
                .pay(
                    job_id,
                    operator_pubkey_b58,
                    payout_address,
                    amount_micro_usdc,
                    receipt,
                )
                .await
        }

        async fn transfer(
            &self,
            transfer_id: Uuid,
            recipient_address: &str,
            amount_micro_usdc: u64,
            memo: &str,
        ) -> Result<TransferRecord, PayoutError> {
            self.inner
                .transfer(transfer_id, recipient_address, amount_micro_usdc, memo)
                .await
        }
    }

    // Echoes like the real happy path unless the buyer's prompt starts
    // with "doom" — the scripted backend failure.
    struct ScriptedExecutor;

    #[async_trait::async_trait]
    impl JobExecutor for ScriptedExecutor {
        async fn execute(
            &self,
            job: &JobEnvelopePayload,
            deadline: Duration,
        ) -> Result<ExecutionOutcome, ExecutorError> {
            let doomed = job
                .input
                .iter()
                .any(|c| matches!(c, Content::Text { text } if text.starts_with("doom")));
            if doomed {
                return Err(ExecutorError::Failed("scripted backend failure".into()));
            }
            EchoExecutor.execute(job, deadline).await
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let coordinator_identity_path = dir.path().join("coordinator-identity.json");
    let journal_path = dir.path().join("journal.jsonl");
    let node_identity_path = dir.path().join("node-identity.json");
    let earnings_path = dir.path().join("earnings.jsonl");
    let rail = Arc::new(MockRail::new());

    let make_state = |payout: Arc<GateablePayout>| {
        let identity =
            LocalIdentity::load_or_create(&coordinator_identity_path, "coordinator@mirror")
                .unwrap();
        let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
        let journal_path = journal_path.clone();
        let rail = rail.clone();
        async move {
            CoordinatorState::with_journal(
                identity,
                CoordinatorConfig {
                    long_poll_timeout: Duration::from_secs(2),
                    default_funding_source: FundingSource::Organic,
                    require_prefunded_buyers: true,
                    fee: covenant_compute_protocol::MarketplaceFee::new(2_000).unwrap(),
                    ..CoordinatorConfig::default()
                },
                Arc::new(AuditReputationSource::new(audit.clone())),
                payout,
                audit,
                &journal_path,
                Some(rail),
            )
            .await
            .unwrap()
        }
    };

    // Life 1 of the coordinator, with the payout rail up.
    let payout = Arc::new(GateablePayout {
        inner: MockPayout::new(),
        down: AtomicBool::new(false),
    });
    let state = make_state(payout.clone()).await;
    let (base_url, server) = spawn_coordinator_abortable(state.clone()).await;
    let http = reqwest::Client::new();

    // Life 1 of the node: identity and earnings book on disk, so a
    // restart keeps the operator's money record.
    let operator = LocalIdentity::load_or_create(&node_identity_path, "operator@mirror").unwrap();
    let profile = cpu_profile(&operator, 1);
    let client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(2),
        1,
    ));
    let registration = client
        .register(RegisterRequest::sign(profile.clone(), payout_addr(12), &operator).unwrap())
        .await
        .unwrap();
    let earnings = Arc::new(JsonlEarningsLedger::open(&earnings_path).unwrap());
    let node = Node::new(
        operator,
        profile,
        client.clone(),
        Arc::new(ScriptedExecutor),
        earnings.clone(),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58: state.coordinator_pubkey_b58(),
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            // The disclosed rate from registration, exactly as the
            // binary wires it — the mirror's net leg tests that this
            // disclosure and the coordinator's captured fee agree.
            fee_bps: registration.fee_bps,
        },
    );
    // The binary reloads the identity for its reconcile task; so does
    // this test.
    let reconcile_identity =
        LocalIdentity::load_or_create(&node_identity_path, "operator@mirror").unwrap();
    let serve_one = || async {
        loop {
            if let Some(outcome) = node.run_once().await.expect("run_once") {
                return outcome;
            }
        }
    };

    let buyer = LocalIdentity::generate("buyer@mirror");
    let buyer_config = BuyerConfig {
        coordinator_url: base_url.clone(),
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    credit_deposit_via_rail(
        &rail,
        &http,
        &buyer_config,
        &buyer,
        10_000,
        "dep-mirror".into(),
    )
    .await;

    // Job 1: served and paid — the rail is up, so the release's
    // first-chance push lands.
    let (paid_job, doomed_job);
    {
        let buyer_task = dispatch_and_verify(
            &http,
            &buyer_config,
            &buyer,
            JobRequest {
                min_reputation_bps: None,
                kind: JobKind::BatchJob,
                model: None,
                gpu_class: None,
                min_vram_gb: None,
                input: vec![Content::text("mirror me")],
                price_micro_usdc: 1_000,
                deadline_ms: 30_000,
            },
        );
        let (buyer_outcome, node_outcome) = tokio::join!(buyer_task, serve_one());
        let outcome = buyer_outcome.expect("job 1 must serve");
        assert_eq!(outcome.receipt.receipt.status, A2ATaskStatus::Ok);
        paid_job = node_outcome.job_id;
    }

    // Job 2: served, released — but the payout rail is down, so the
    // coordinator owes the operator and both books must show it.
    payout.down.store(true, Ordering::SeqCst);
    let outstanding_job;
    {
        let buyer_task = dispatch_and_verify(
            &http,
            &buyer_config,
            &buyer,
            JobRequest {
                min_reputation_bps: None,
                kind: JobKind::BatchJob,
                model: None,
                gpu_class: None,
                min_vram_gb: None,
                input: vec![Content::text("push fails after me")],
                price_micro_usdc: 1_000,
                deadline_ms: 30_000,
            },
        );
        let (buyer_outcome, node_outcome) = tokio::join!(buyer_task, serve_one());
        buyer_outcome.expect("a payout outage must not cost the buyer the result");
        outstanding_job = node_outcome.job_id;
    }

    // Job 3: the executor fails — refund, fault, and NO credit on the
    // node book, whatever the delivery acked.
    {
        let buyer_task = dispatch_and_verify(
            &http,
            &buyer_config,
            &buyer,
            JobRequest {
                min_reputation_bps: None,
                kind: JobKind::BatchJob,
                model: None,
                gpu_class: None,
                min_vram_gb: None,
                input: vec![Content::text("doom: backend gives out")],
                price_micro_usdc: 1_000,
                deadline_ms: 30_000,
            },
        );
        let (buyer_outcome, node_outcome) = tokio::join!(buyer_task, serve_one());
        let err = buyer_outcome.expect_err("a failed job must not verify as served");
        doomed_job = match err {
            BuyerError::NotServed {
                job_id, ref status, ..
            } if status == "failed" => job_id,
            other => panic!("expected NotServed(failed), got {other:?}"),
        };
        assert_eq!(node_outcome.job_id, doomed_job);
    }

    // Job 4: matched but never accepted — the node is not polling —
    // and swept as a deadline expiry, the refund arm that never
    // touches the node at all.
    let expired_job = Uuid::new_v4();
    let expiry_envelope = signed_envelope(
        &buyer,
        expired_job,
        1_000,
        1_000,
        epoch_ms(),
        "mirror-expiry",
    );
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&expiry_envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let swept = sweep_expired(&state, epoch_ms() + 60_000).await;
    assert!(
        swept.contains(&expired_job),
        "the expiry sweep must refund the never-accepted job"
    );

    // First reconcile pass over the real wire: exactly the paid job
    // flips; the outstanding one stays owed.
    let rows = client.operator_jobs(&reconcile_identity).await.unwrap();
    assert_eq!(rows.len(), 4, "all four jobs are this operator's business");
    assert_eq!(
        reconcile_paid_rows(earnings.as_ref(), &rows, epoch_ms()).await,
        1
    );
    let entries = earnings.recent(100).await;
    assert_wire_mirror(&rows, &entries, &state, "rail down").await;
    assert_eq!(
        earnings.unpaid_total_micro_usdc().await,
        800,
        "the outstanding job's net (1000 gross - 20% fee) is still owed"
    );

    // The rail heals: the retry sweep pushes the owed payout, and the
    // next reconcile pass flips exactly that entry.
    payout.down.store(false, Ordering::SeqCst);
    let repushed = sweep_unpaid(&state).await;
    assert_eq!(repushed, vec![outstanding_job]);
    let rows = client.operator_jobs(&reconcile_identity).await.unwrap();
    assert_eq!(
        reconcile_paid_rows(earnings.as_ref(), &rows, epoch_ms()).await,
        1
    );
    let entries = earnings.recent(100).await;
    assert_wire_mirror(&rows, &entries, &state, "rail healed").await;
    assert_eq!(earnings.unpaid_total_micro_usdc().await, 0);
    assert_eq!(
        entries
            .iter()
            .filter(|e| e.status == EarningsStatus::Paid)
            .count(),
        2
    );
    assert!(entries.iter().all(|e| e.job_id != doomed_job
        && e.job_id != expired_job
        && (e.job_id == paid_job || e.job_id == outstanding_job)));

    let feed_before = feed_snapshot(&rows);
    let entries_before = entries;

    // Both parties die. The coordinator replays its journal behind a
    // fresh payout backend; the node reopens its ledger file. The
    // mirror must come back identical on both sides — the payout
    // confirmations replay from the journal, not from the backend, and
    // nothing gets re-pushed or re-flipped.
    server.abort();
    drop(state);
    drop(node);
    let payout2 = Arc::new(GateablePayout {
        inner: MockPayout::new(),
        down: AtomicBool::new(false),
    });
    let state2 = make_state(payout2.clone()).await;
    let (base_url2, _server2) = spawn_coordinator_abortable(state2.clone()).await;
    let client2 = HttpCoordinatorClient::with_config(base_url2, Duration::from_secs(2), 1);
    let earnings2 = JsonlEarningsLedger::open(&earnings_path).unwrap();

    let rows_after = client2.operator_jobs(&reconcile_identity).await.unwrap();
    assert_eq!(
        feed_snapshot(&rows_after),
        feed_before,
        "journal replay must rebuild the operator feed exactly"
    );
    let entries_after = earnings2.recent(100).await;
    assert_eq!(
        entries_after, entries_before,
        "the reopened ledger must carry the identical money record"
    );
    assert_eq!(
        reconcile_paid_rows(&earnings2, &rows_after, epoch_ms()).await,
        0,
        "a reconcile against the replayed books finds nothing left to flip"
    );
    assert_wire_mirror(&rows_after, &entries_after, &state2, "after both restarts").await;
    assert!(
        payout2.inner.records().is_empty(),
        "replay must not re-push a payout the journal already confirms"
    );
}

/// Everything `/metrics` says about the books — the scrapable face of
/// every ledger. The liveness gauge is excluded only because it is
/// clock-derived, not book-derived.
async fn compute_metric_lines(http: &reqwest::Client, base_url: &str) -> Vec<String> {
    let body = http
        .get(format!("{base_url}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    body.lines()
        .filter(|l| l.starts_with("compute_") && !l.starts_with("compute_operators_live"))
        .map(str::to_string)
        .collect()
}

async fn assert_wire_answer(
    req: reqwest::RequestBuilder,
    allow_2xx: bool,
    label: String,
    violations: &mut Vec<String>,
) {
    match tokio::time::timeout(Duration::from_secs(10), req.send()).await {
        Err(_) => violations.push(format!("{label}: no answer within 10s")),
        Ok(Err(e)) => violations.push(format!("{label}: connection severed ({e})")),
        Ok(Ok(resp)) => {
            let status = resp.status().as_u16();
            let clean_refusal = (400..500).contains(&status);
            let allowed_answer = allow_2xx && (200..300).contains(&status);
            if !clean_refusal && !allowed_answer {
                violations.push(format!("{label}: status {status}"));
            }
        }
    }
}

/// C9 hostile-wire law over the whole route table at once: garbage a
/// public port actually receives — raw bytes, wrong shapes, wrong
/// types, deep nesting, oversized bodies and params, traversal-shaped
/// path segments, forged auth — always gets a clean 4xx back. Never a
/// 2xx (garbage must not be accepted), never a 5xx (a parse is not an
/// internal error), never a severed connection (no handler panics),
/// and the barrage as a whole moves not one micro-USDC in any book.
/// The refusal ARMS are pinned one by one across this file; this pins
/// the posture of the surface, so a future handler that unwraps its
/// way to a panic or leaks a refusal as a 500 fails here no matter
/// which route it hides behind.
#[tokio::test]
async fn hostile_wire_garbage_gets_4xx_moves_no_money_and_the_service_keeps_serving() {
    let (state, _payout) = new_coordinator_state(Duration::from_millis(200));
    let state_handle = state.clone();
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state).await;
    let http = reqwest::Client::new();

    // If a route is added or removed, this count moves — extend the
    // barrage below to cover the new surface before bumping it.
    let router_source =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/http.rs")).unwrap();
    assert_eq!(
        router_source.matches(".route(").count(),
        42,
        "the route table changed — teach the hostile-wire barrage the new route first"
    );

    // A real operator and one settled paid job, so every book holds
    // live numbers before the barrage starts.
    let operator_identity = LocalIdentity::generate("operator@hostile-wire");
    let operator_key = operator_identity.agent_id().pubkey_base58();
    let profile = cpu_profile(&operator_identity, 1_000);
    let coordinator_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        2,
    ));
    let register_resp = coordinator_client
        .register(
            RegisterRequest::sign(profile.clone(), payout_addr(2), &operator_identity).unwrap(),
        )
        .await
        .unwrap();
    assert!(register_resp.accepted);
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

    let buyer = LocalIdentity::generate("buyer@hostile-wire");
    let before_job = Uuid::new_v4();
    let envelope = signed_envelope(
        &buyer,
        before_job,
        1_000,
        30_000,
        epoch_ms(),
        "hostile-before",
    );
    let submitted = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submitted.status(), reqwest::StatusCode::ACCEPTED);
    node.run_once()
        .await
        .expect("serve the pre-barrage job")
        .expect("the job was offered to the only operator");
    assert_eq!(
        state_handle.escrow().status(before_job).await.unwrap(),
        EscrowStatus::Released
    );

    let books_before = compute_metric_lines(&http, &base_url).await;
    assert!(!books_before.is_empty(), "the money books are scrapable");

    let long_param = "x".repeat(600);
    let hostile_params = [
        "not-a-uuid",
        "..%2F..%2Fetc%2Fpasswd",
        "%00%00",
        "🦀🦀🦀",
        long_param.as_str(),
    ];
    let deep_nesting = "[".repeat(300) + &"]".repeat(300);
    let wrong_types = r#"{"payload":42,"signature_b58":{"x":null}}"#;
    let garbage_bodies: [(&str, &[u8], &str); 7] = [
        ("empty-body", b"", "application/json"),
        ("raw-bytes", b"\xff\xfe{{{ not json", "application/json"),
        ("empty-object", b"{}", "application/json"),
        ("array-not-object", b"[1,2,3]", "application/json"),
        ("wrong-types", wrong_types.as_bytes(), "application/json"),
        ("deep-nesting", deep_nesting.as_bytes(), "application/json"),
        ("wrong-content-type", b"{}", "text/plain"),
    ];

    let mut violations: Vec<String> = Vec::new();

    // Input-less GETs: a hostile query string is ignorable, so a 200
    // is a legitimate answer; a 5xx or a hang never is.
    for path in [
        "/health",
        "/metrics",
        "/federation/deposit-info",
        "/federation/bond-info",
        "/federation/subsidy",
        "/federation/fees",
        "/federation/capacity",
        "/federation/partners",
        // Public proof feed: opt-in, so off in this fixture it 404s, but
        // a hostile query is ignorable either way — never a 5xx.
        "/proof/receipts",
        "/proof/batch",
        // Admin-gated: hostile queries with no bearer land on the
        // fail-closed 401, never a 5xx or a list.
        "/admin/transfers",
    ] {
        let url = format!("{base_url}{path}?x=..%2F..&y=%00&z={long_param}");
        assert_wire_answer(
            http.get(&url),
            true,
            format!("GET {path} (hostile query)"),
            &mut violations,
        )
        .await;
    }

    // Param routes: a garbage segment makes the question itself
    // garbage — except reputation, where the standing of an unknown
    // key is a valid public question and answers a neutral view.
    for hp in hostile_params {
        for (path, allow_2xx) in [
            (format!("/federation/operators/{hp}/next-job"), false),
            (format!("/federation/operators/{hp}/reputation"), true),
            (format!("/federation/operators/{hp}/jobs"), false),
            (format!("/federation/operators/{hp}/bond"), false),
            (format!("/federation/jobs/{hp}/receipt"), false),
            (format!("/proof/receipts/{hp}"), false),
            (format!("/proof/batch/{hp}"), false),
            (format!("/federation/jobs/{hp}/lease"), false),
            (format!("/federation/jobs/{hp}/stream"), false),
            (format!("/federation/buyers/{hp}/balance"), false),
            (format!("/federation/buyers/{hp}/withdrawals"), false),
            (format!("/federation/buyers/{hp}/jobs"), false),
            // Vault reads: off by default, so every owner and label is a
            // 404; an enabled vault would 401 an unsigned read. Never 2xx.
            (format!("/vault/{hp}/secrets"), false),
            (format!("/vault/{hp}/secret/{hp}"), false),
            // Bundles: none is stored unless agent work runs, and a
            // garbage digest never names one.
            (format!("/federation/bundles/{hp}"), false),
        ] {
            assert_wire_answer(
                http.get(format!("{base_url}{path}")),
                allow_2xx,
                format!("GET {path}"),
                &mut violations,
            )
            .await;
        }
    }

    // POST routes × garbage body classes. Param'd routes get both a
    // well-formed unknown id and a hostile segment.
    let unknown_job = Uuid::new_v4();
    let post_paths = [
        "/federation/operators/register".to_string(),
        "/federation/operators/heartbeat".to_string(),
        "/federation/jobs".to_string(),
        format!("/federation/jobs/{unknown_job}/accept"),
        format!("/federation/jobs/{unknown_job}/result"),
        format!("/federation/jobs/{unknown_job}/stream"),
        format!("/federation/jobs/{unknown_job}/dispute"),
        format!("/federation/jobs/{unknown_job}/cancel"),
        format!("/federation/jobs/{unknown_job}/close"),
        format!("/federation/jobs/{unknown_job}/hidden"),
        format!("/federation/jobs/{}/close", hostile_params[1]),
        format!("/federation/jobs/{}/hidden", hostile_params[1]),
        format!("/federation/jobs/{}/accept", hostile_params[1]),
        "/federation/buyers/deposit".to_string(),
        "/federation/buyers/withdraw".to_string(),
        "/federation/operators/bond".to_string(),
        "/federation/operators/unbond".to_string(),
        "/federation/partners/no-such-code/payouts".to_string(),
        format!("/federation/partners/{long_param}/payouts"),
        // Body-less admin route: garbage bodies are ignorable, the
        // missing/forged bearer is not — every barrage shape lands on
        // the fail-closed 401, and the subsidy latch stays untouched.
        "/federation/subsidy/close".to_string(),
        // Admin-gated resolve: garbage bodies die in the JSON layer,
        // a well-formed one on the missing bearer — clean 4xx either
        // way, and no bracket moves. Param'd like the job routes: an
        // unknown id and a hostile segment both die clean.
        format!("/admin/transfers/{unknown_job}/resolve"),
        format!("/admin/transfers/{}/resolve", hostile_params[1]),
        // Vault store: off by default, so a garbage body dies on the 404
        // before the signature check; an enabled vault would 401 it.
        format!("/vault/{}/secret/deploy", hostile_params[0]),
    ];
    for path in &post_paths {
        for (class, body, content_type) in garbage_bodies {
            assert_wire_answer(
                http.post(format!("{base_url}{path}"))
                    .header("content-type", content_type)
                    .body(body.to_vec()),
                false,
                format!("POST {path} ({class})"),
                &mut violations,
            )
            .await;
        }
    }

    // A bundle upload with no signature, or to a coordinator storing none,
    // is a 4xx and stores nothing.
    for hp in hostile_params {
        assert_wire_answer(
            http.put(format!("{base_url}/federation/bundles/{hp}"))
                .body(b"\xff\xfe not a bundle".to_vec()),
            false,
            format!("PUT /federation/bundles/{hp}"),
            &mut violations,
        )
        .await;
    }

    // DELETE shares the vault secret path with GET and POST; an unsigned
    // one is a 4xx like the rest and moves nothing.
    for hp in hostile_params {
        assert_wire_answer(
            http.delete(format!("{base_url}/vault/{hp}/secret/deploy")),
            false,
            format!("DELETE /vault/{hp}/secret/deploy"),
            &mut violations,
        )
        .await;
    }

    // Shape-valid but forged: a well-formed envelope whose signature
    // is corrupt sails past every extractor and must die in the
    // handler's own verify — the arm that answers refusals, not the
    // JSON layer. This is the probe that catches a refusal leaking
    // back out as an internal error.
    let mut forged = serde_json::to_value(signed_envelope(
        &buyer,
        Uuid::new_v4(),
        1_000,
        30_000,
        epoch_ms(),
        "hostile-forged",
    ))
    .unwrap();
    let sig = forged["signature_b58"].as_str().unwrap();
    let flip = if sig.starts_with('2') { '3' } else { '2' };
    forged["signature_b58"] = serde_json::Value::String(format!("{flip}{}", &sig[1..]));
    assert_wire_answer(
        http.post(format!("{base_url}/federation/jobs"))
            .json(&forged),
        false,
        "POST /federation/jobs (forged signature)".to_string(),
        &mut violations,
    )
    .await;

    // An oversized body must die at the size limit, not in a handler.
    let huge = format!(r#"{{"payload":"{}"}}"#, "x".repeat(3 * 1024 * 1024));
    assert_wire_answer(
        http.post(format!("{base_url}/federation/jobs"))
            .header("content-type", "application/json")
            .body(huge),
        false,
        "POST /federation/jobs (3MiB body)".to_string(),
        &mut violations,
    )
    .await;

    // Real resources behind forged auth: a garbage bearer on the
    // operator's own queue, garbage read-signature headers on a real
    // receipt. The resource existing must not soften the refusal.
    assert_wire_answer(
        http.get(format!(
            "{base_url}/federation/operators/{operator_key}/next-job"
        ))
        .header("authorization", "Bearer garbage-session"),
        false,
        "GET next-job (real operator, forged session)".to_string(),
        &mut violations,
    )
    .await;
    assert_wire_answer(
        http.get(format!("{base_url}/federation/jobs/{before_job}/receipt"))
            .header(covenant_compute_protocol::READ_SIGNED_AT_HEADER, "%%%")
            .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, "%%%"),
        false,
        "GET receipt (real job, garbage read signature)".to_string(),
        &mut violations,
    )
    .await;

    // Wrong methods bounce off the router itself.
    assert_wire_answer(
        http.delete(format!("{base_url}/federation/jobs")),
        false,
        "DELETE /federation/jobs".to_string(),
        &mut violations,
    )
    .await;
    assert_wire_answer(
        http.put(format!("{base_url}/health")),
        false,
        "PUT /health".to_string(),
        &mut violations,
    )
    .await;

    assert!(
        violations.is_empty(),
        "hostile wire violations:\n{}",
        violations.join("\n")
    );

    // The barrage moved nothing: every scrapable book reads the same.
    let books_after = compute_metric_lines(&http, &base_url).await;
    assert_eq!(
        books_before, books_after,
        "a garbage barrage must not move any book"
    );

    // And the service it hit is still the service: a real paid job
    // settles end to end afterwards.
    let after_job = Uuid::new_v4();
    let envelope = signed_envelope(
        &buyer,
        after_job,
        1_000,
        30_000,
        epoch_ms(),
        "hostile-after",
    );
    let submitted = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submitted.status(), reqwest::StatusCode::ACCEPTED);
    node.run_once()
        .await
        .expect("serve the post-barrage job")
        .expect("the job was offered to the only operator");
    assert_eq!(
        state_handle.escrow().status(after_job).await.unwrap(),
        EscrowStatus::Released
    );
}

/// The version-handshake law, passive half: every reply — success,
/// refusal, even a 404 off the route table — names the coordinator's
/// wire version, so any client can tell how new the service is from
/// whatever answer it already got.
#[tokio::test]
async fn every_reply_names_the_coordinators_wire_version() {
    let (state, _payout) = new_coordinator_state(Duration::from_millis(200));
    let base_url = spawn_coordinator(state).await;
    let http = reqwest::Client::new();

    let expect = covenant_compute_protocol::PROTOCOL_VERSION.to_string();
    for (label, resp) in [
        (
            "GET /health",
            http.get(format!("{base_url}/health")).send().await.unwrap(),
        ),
        (
            "GET /federation/capacity",
            http.get(format!("{base_url}/federation/capacity"))
                .send()
                .await
                .unwrap(),
        ),
        (
            "POST /federation/jobs (garbage)",
            http.post(format!("{base_url}/federation/jobs"))
                .body("not json")
                .header("content-type", "application/json")
                .send()
                .await
                .unwrap(),
        ),
        (
            "GET /nowhere (404)",
            http.get(format!("{base_url}/nowhere"))
                .send()
                .await
                .unwrap(),
        ),
    ] {
        let stamped = resp
            .headers()
            .get(covenant_compute_protocol::PROTOCOL_VERSION_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_else(|| panic!("{label} reply carries no wire-version header"));
        assert_eq!(stamped, expect, "{label}");
    }
}

/// The version-handshake law, active half: a deployment that raised
/// its wire floor refuses older `/federation/*` clients with 426 and
/// BOTH numbers in the body — an operator on a stale node reads
/// "upgrade", never a shape error — while `/health` and `/metrics`
/// stay open to versionless probes and a garbled version header is
/// refused as hostile wire everywhere.
#[tokio::test]
async fn a_raised_wire_floor_refuses_older_clients_by_name() {
    let identity = LocalIdentity::generate("coordinator@e2e");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    let config = CoordinatorConfig {
        long_poll_timeout: Duration::from_millis(200),
        min_protocol: 2,
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::new(identity, config, reputation, payout, audit);
    let base_url = spawn_coordinator(state).await;
    let http = reqwest::Client::new();
    let capacity = format!("{base_url}/federation/capacity");
    let header = covenant_compute_protocol::PROTOCOL_VERSION_HEADER;

    // Versionless (a pre-versioning binary, bare curl) counts as 0.
    let refused = http.get(&capacity).send().await.unwrap();
    assert_eq!(refused.status().as_u16(), 426);
    assert_eq!(
        refused.headers().get(header).unwrap().to_str().unwrap(),
        covenant_compute_protocol::PROTOCOL_VERSION.to_string(),
        "even the refusal names the coordinator's own version"
    );
    let body: serde_json::Value = refused.json().await.unwrap();
    let message = body["error"].as_str().unwrap();
    for needle in ["wire protocol 0", "floor 2", "upgrade"] {
        assert!(
            message.contains(needle),
            "refusal must name the mismatch: missing {needle:?} in {message:?}"
        );
    }

    // Declaring below the floor refuses; at or above it passes — a
    // floor, not an equality check, so newer clients keep working.
    for (declared, expect_status) in [("1", 426u16), ("2", 200), ("7", 200)] {
        let resp = http
            .get(&capacity)
            .header(header, declared)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), expect_status, "declared {declared}");
    }

    // Deploy probes and scrapers never learned the header: exempt.
    for path in ["/health", "/metrics"] {
        let resp = http.get(format!("{base_url}{path}")).send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 200, "{path} must not floor");
    }

    // A version that isn't a u32 is hostile wire on any path.
    let garbled = http
        .get(format!("{base_url}/health"))
        .header(header, "banana")
        .send()
        .await
        .unwrap();
    assert_eq!(garbled.status().as_u16(), 400);
    let body: serde_json::Value = garbled.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains(header));
}

/// Anti-faucet (invariant #1) × C8: a canary probe is coordinator-
/// manufactured traffic with a synthetic buyer, funded from the
/// bootstrap subsidy — not a real sale. It must accrue NO supply-side
/// partner rev-share, even against an operator that registered a partner
/// code, or the coordinator would pay an external partner (in
/// withdrawable money) for work it commissioned itself. The operator is
/// still paid its net (a probe is a real job it ran); only the partner
/// accrual must be absent. The settled record proves the fee/partner
/// path ran (a fee was captured) yet carved zero share.
#[tokio::test]
async fn a_canary_probe_accrues_no_partner_rev_share() {
    use covenant_compute_coordinator::{CanaryConfig, CanaryProber, PartnerConfig, SubsidyPolicy};
    use covenant_compute_node::{ExecutionOutcome, ExecutorError, JobExecutor};

    // Follows the probe instruction (returns the prompt's last token, the
    // canary nonce): a passing probe that releases and pays — the settle
    // path where a supply-side share would be carved.
    struct HonestExecutor;
    #[async_trait::async_trait]
    impl JobExecutor for HonestExecutor {
        async fn execute(
            &self,
            job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<ExecutionOutcome, ExecutorError> {
            let Some(Content::Text { text }) = job.input.first() else {
                return Err(ExecutorError::Failed("no text input".into()));
            };
            Ok(ExecutionOutcome {
                output: vec![Content::text(
                    text.rsplit(' ').next().unwrap_or_default().to_string(),
                )],
                wall_ms: 1,
                tokens_in: Some(1),
                tokens_out: Some(1),
                finish_reason: None,
            })
        }
    }

    let identity = LocalIdentity::generate("coordinator@e2e");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    // A live marketplace fee and a configured supply-side partner: the
    // exact configuration under which a leaked probe share is non-zero
    // (fee 20% of 1_000 = 200, share 60% of 200 = 120 in the buggy path).
    let mut partners = std::collections::HashMap::new();
    partners.insert(
        "partner-canary".to_string(),
        PartnerConfig::new("partner-canary-address".into(), 6_000).unwrap(),
    );
    let config = CoordinatorConfig {
        long_poll_timeout: Duration::from_secs(2),
        fee: covenant_compute_protocol::MarketplaceFee::new(2_000).unwrap(),
        partners,
        subsidy_policy: Some(SubsidyPolicy::new(10_000, 1_000_000).unwrap()),
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::new(identity, config, reputation, payout.clone(), audit.clone());
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state.clone()).await;

    let operator_identity = LocalIdentity::generate("probed@e2e");
    let operator_key = operator_identity.agent_id().pubkey_base58();
    let profile = CapabilityProfile {
        models_served: vec!["canary-model".into()],
        job_kinds: vec![JobKind::InferenceCall],
        ..cpu_profile(&operator_identity, 1_000)
    };
    let client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        2,
    ));
    let register_req = RegisterRequest::sign_referred(
        profile.clone(),
        payout_for("probed@e2e"),
        Some("partner-canary".into()),
        &operator_identity,
    )
    .unwrap();
    assert!(client.register(register_req).await.unwrap().accepted);

    let node = Node::new(
        operator_identity,
        profile,
        client,
        Arc::new(HonestExecutor),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    );
    tokio::spawn(async move {
        loop {
            let _ = node.run_once().await;
        }
    });

    let prober = CanaryProber::new(
        state.clone(),
        CanaryConfig {
            max_price_micro_usdc: 10_000,
            deadline_ms: 30_000,
        },
    );
    let mut probe_id = None;
    for _ in 0..80 {
        let report = prober.tick().await;
        if let Some((job_id, _)) = report.dispatched {
            probe_id = Some(job_id);
        }
        if let Some(id) = probe_id {
            if report.judged.iter().any(|(j, _, _)| *j == id) {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let probe_id = probe_id.expect("a probe was dispatched to the only operator");

    // Let settlement finish: release -> payout -> Completed.
    for _ in 0..50 {
        let settled = matches!(
            state.jobs().get(probe_id).map(|r| r.phase),
            Some(covenant_compute_coordinator::JobPhase::Completed)
        ) && payout
            .records()
            .iter()
            .any(|r| r.operator_pubkey_b58 == operator_key);
        if settled {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The probe paid the operator its net — a canary is a real job.
    assert!(
        payout
            .records()
            .iter()
            .any(|r| r.operator_pubkey_b58 == operator_key),
        "the probed operator was paid its net for the completed probe"
    );

    // The fee path ran (a fee was captured, so the partner config is
    // unquestionably live) yet no supply-side share was carved and the
    // record carries no referral code.
    let record = state.jobs().get(probe_id).expect("probe record");
    assert_eq!(
        record.phase,
        covenant_compute_coordinator::JobPhase::Completed
    );
    assert!(
        record.fee_micro_usdc > 0,
        "settlement captured a marketplace fee, so the fee/partner path was exercised"
    );
    assert_eq!(
        record.referral_code, None,
        "a canary carries no supply-side referral code"
    );
    assert_eq!(
        record.partner_share_micro_usdc, 0,
        "no partner share is carved from a subsidy-funded probe"
    );

    // Nothing withdrawable accrued, and no accrual audit row exists.
    assert!(
        !state
            .jobs()
            .partner_accruals()
            .contains_key("partner-canary"),
        "the partner earned nothing from a coordinator-manufactured probe"
    );
    let events = audit.recent(usize::MAX).await.unwrap();
    assert!(
        !events.iter().any(|e| matches!(
            &e.kind,
            AuditKind::ComputePartnerShareAccrued { job_id, .. } if *job_id == probe_id
        )),
        "no supply-side partner accrual is recorded for a probe"
    );
}

/// Anti-faucet (invariant #1) × C8, mirror path: a redundancy mirror is
/// the same coordinator-manufactured, subsidy-funded traffic as a
/// canary, so it too must accrue NO supply-side partner rev-share — even
/// when the mirror operator registered a partner code. The mirrors
/// settle and pay their operators (real verified jobs) but carve zero
/// share; the source operator carries no code, so the partner books stay
/// empty.
#[tokio::test]
async fn a_redundancy_mirror_accrues_no_partner_rev_share() {
    use covenant_compute_coordinator::{
        PartnerConfig, RedundancyConfig, RedundancySampler, SubsidyPolicy,
    };
    use covenant_compute_node::{ExecutionOutcome, ExecutorError, JobExecutor};

    // Honest batch runner: every node returns the input verbatim, so all
    // receipts agree and every mirror releases and pays — the settle path
    // where a share would leak.
    struct HonestBatch;
    #[async_trait::async_trait]
    impl JobExecutor for HonestBatch {
        async fn execute(
            &self,
            job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<ExecutionOutcome, ExecutorError> {
            let Some(Content::Text { text }) = job.input.first() else {
                return Err(ExecutorError::Failed("no text input".into()));
            };
            Ok(ExecutionOutcome {
                output: vec![Content::text(text.clone())],
                wall_ms: 1,
                tokens_in: None,
                tokens_out: None,
                finish_reason: None,
            })
        }
    }

    let identity = LocalIdentity::generate("coordinator@e2e");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    let mut partners = std::collections::HashMap::new();
    partners.insert(
        "partner-mirror".to_string(),
        PartnerConfig::new("partner-mirror-address".into(), 6_000).unwrap(),
    );
    let config = CoordinatorConfig {
        long_poll_timeout: Duration::from_secs(2),
        fee: covenant_compute_protocol::MarketplaceFee::new(2_000).unwrap(),
        partners,
        subsidy_policy: Some(SubsidyPolicy::new(10_000, 1_000_000).unwrap()),
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::new(identity, config, reputation, payout.clone(), audit.clone());
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state.clone()).await;

    // Source asks cheapest (wins the organic match) and carries no code;
    // the two mirror operators register WITH the partner code, so a
    // leaked mirror share would land on partner-mirror.
    let mut mirror_operators: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (display, ask, code) in [
        ("source@e2e", 800u64, None),
        ("mirror-a@e2e", 1_000, Some("partner-mirror")),
        ("mirror-b@e2e", 1_000, Some("partner-mirror")),
    ] {
        let operator_identity = LocalIdentity::generate(display);
        let profile = cpu_profile(&operator_identity, ask);
        let client = Arc::new(HttpCoordinatorClient::with_config(
            base_url.clone(),
            Duration::from_secs(5),
            2,
        ));
        let register_req = RegisterRequest::sign_referred(
            profile.clone(),
            payout_for(display),
            code.map(str::to_string),
            &operator_identity,
        )
        .unwrap();
        assert!(client.register(register_req).await.unwrap().accepted);
        if code.is_some() {
            mirror_operators.insert(operator_identity.agent_id().pubkey_base58());
        }
        let node = Node::new(
            operator_identity,
            profile,
            client,
            Arc::new(HonestBatch),
            Arc::new(InMemoryEarningsLedger::new()),
            Arc::new(InMemoryAuditLog::new()),
            NodeConfig {
                coordinator_pubkey_b58: coordinator_pubkey_b58.clone(),
                max_in_flight: 4,
                preempt_grace: Duration::from_secs(2),
                fee_bps: 0,
            },
        );
        tokio::spawn(async move {
            loop {
                let _ = node.run_once().await;
            }
        });
    }

    // A real buyer's batch job -> matched to the cheapest (source).
    let buyer = LocalIdentity::generate("buyer@e2e");
    let source_job = Uuid::new_v4();
    let envelope = signed_envelope(
        &buyer,
        source_job,
        1_000,
        30_000,
        epoch_ms(),
        "mirror-rev-share",
    );
    let http = reqwest::Client::new();
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);

    for _ in 0..100 {
        if matches!(
            state.jobs().get(source_job).map(|r| r.phase),
            Some(covenant_compute_coordinator::JobPhase::Completed)
        ) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        state.jobs().get(source_job).unwrap().phase,
        covenant_compute_coordinator::JobPhase::Completed
    );

    // Sample it: two mirrors dispatched to the partner-coded operators.
    let sampler = RedundancySampler::new(state.clone(), RedundancyConfig::default());
    let mut mirror_ids: Vec<(Uuid, String)> = Vec::new();
    for _ in 0..100 {
        let report = sampler.tick().await;
        if let Some((_src, mirrors)) = report.dispatched {
            mirror_ids = mirrors;
        }
        let all_settled = mirror_ids.len() == 2
            && mirror_ids.iter().all(|(id, _)| {
                matches!(
                    state.jobs().get(*id).map(|r| r.phase),
                    Some(covenant_compute_coordinator::JobPhase::Completed)
                )
            });
        if all_settled {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(mirror_ids.len(), 2, "both mirror operators drew a mirror");

    // Each mirror settled (a fee was captured, so the partner path ran)
    // but carved zero share and carries no referral code.
    for (mirror_id, operator) in &mirror_ids {
        assert!(
            mirror_operators.contains(operator),
            "a mirror went to a partner-coded operator"
        );
        let record = state.jobs().get(*mirror_id).unwrap();
        assert_eq!(
            record.phase,
            covenant_compute_coordinator::JobPhase::Completed
        );
        assert!(
            record.fee_micro_usdc > 0,
            "settlement captured a fee, so the fee/partner path was exercised"
        );
        assert_eq!(
            record.referral_code, None,
            "a mirror carries no supply-side referral code"
        );
        assert_eq!(
            record.partner_share_micro_usdc, 0,
            "no partner share is carved from a subsidy-funded mirror"
        );
    }

    assert!(
        !state
            .jobs()
            .partner_accruals()
            .contains_key("partner-mirror"),
        "no partner rev-share accrues from coordinator-manufactured mirror traffic"
    );
}

/// A job result carries the operator's output inline, and a synthesized
/// speech clip — or a chunk of transcription audio — is easily several
/// megabytes, past axum's 2 MiB default body limit. The coordinator raises
/// its two payload-carrying endpoints to the network's 8 MiB frame cap, so
/// a real result reaches the handler that verifies it instead of a 413 at
/// the transport. This pins that window from both sides: a 3 MiB body is
/// accepted past the old default (then fails on its own merits, naming an
/// unknown job), while a body past the frame cap is still refused, so the
/// endpoint stays bounded.
#[tokio::test]
async fn a_large_result_body_transits_up_to_the_frame_cap() {
    let (state, _payout) = new_coordinator_state(Duration::from_millis(50));
    let base_url = spawn_coordinator(state).await;
    let http = reqwest::Client::new();
    let job_id = Uuid::new_v4();
    let result_url = format!("{base_url}/federation/jobs/{job_id}/result");

    // Over the 2 MiB axum default, under the 8 MiB frame cap: the body must
    // reach the handler, which rejects it as an unknown job — anything but
    // a 413 proves the limit let it through.
    let over_the_default = serde_json::json!({
        "output": [{ "type": "text", "text": "A".repeat(3 * 1024 * 1024) }]
    });
    let resp = http
        .post(&result_url)
        .json(&over_the_default)
        .send()
        .await
        .unwrap();
    assert_ne!(
        resp.status(),
        reqwest::StatusCode::PAYLOAD_TOO_LARGE,
        "a multi-megabyte result must reach the handler, not 413 at the body limit"
    );

    // Past the frame cap the limit still bites, so a runaway body cannot
    // exhaust the coordinator's memory.
    let over_the_frame = serde_json::json!({
        "output": [{ "type": "text", "text": "A".repeat(9 * 1024 * 1024) }]
    });
    let resp = http
        .post(&result_url)
        .json(&over_the_frame)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::PAYLOAD_TOO_LARGE,
        "a body past the 8 MiB frame cap is still refused"
    );
}

/// Builds a signed `LeaseSession` envelope: the buyer escrows the whole
/// window (`rate × duration`, the price `verify` demands) and gets back
/// only the seconds the session did not run.
fn signed_lease_envelope(
    buyer: &LocalIdentity,
    job_id: Uuid,
    rate_micro_usdc_per_sec: u64,
    max_duration_secs: u64,
    issued_at_ms: u64,
    idem_key: &str,
) -> SignedJobEnvelope {
    let terms = covenant_compute_protocol::LeaseTerms {
        max_duration_secs,
        rate_micro_usdc_per_sec,
        client_public_key: Some("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5 buyer@e2e".into()),
    };
    let ceiling = terms.max_price_micro_usdc().unwrap();
    let payload = JobEnvelopePayload {
        job_id,
        buyer: buyer.agent_id(),
        kind: JobKind::LeaseSession,
        capability_requirement: CapabilityRequirement {
            gpu_class: None,
            min_vram_gb: None,
            model_id: None,
            kind: JobKind::LeaseSession,
            max_duration_secs: max_duration_secs as u32,
            min_reputation_bps: None,
        },
        input: vec![covenant_compute_protocol::lease_input(terms).unwrap()],
        price_micro_usdc: ceiling,
        deadline_ms: max_duration_secs * 1_000 + covenant_compute_protocol::LEASE_DEADLINE_SLACK_MS,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, idem_key),
        issued_at_ms,
        referral_code: None,
        stream: false,
    };
    SignedJobEnvelope::sign(payload, buyer).unwrap()
}

/// Registers `identity` and returns its operator session bearer.
async fn register_session(
    http: &reqwest::Client,
    base_url: &str,
    identity: &LocalIdentity,
    profile: &covenant_compute_protocol::CapabilityProfile,
    payout: String,
) -> String {
    let resp = http
        .post(format!("{base_url}/federation/operators/register"))
        .json(&RegisterRequest::sign(profile.clone(), payout, identity).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    resp.json::<serde_json::Value>().await.unwrap()["operator_session"]
        .as_str()
        .unwrap()
        .to_string()
}

/// An operator-signed `Ok` receipt over a lease envelope's output —
/// what a node submits when its session ends.
fn lease_receipt(
    job_id: Uuid,
    operator: &LocalIdentity,
    envelope: &SignedJobEnvelope,
    output: &[Content],
) -> SignedWorkReceipt {
    SignedWorkReceipt::sign(
        WorkReceiptPayload {
            job_id,
            operator: operator.agent_id(),
            job_hash_hex: covenant_compute_protocol::output_hash_hex(&envelope.payload.input),
            result_hash_hex: covenant_compute_protocol::output_hash_hex(output),
            meter: JobMeter {
                wall_ms: 1,
                tokens_in: None,
                tokens_out: None,
                gpu_seconds: None,
                finish_reason: None,
            },
            price_micro_usdc: envelope.payload.price_micro_usdc,
            status: A2ATaskStatus::Ok,
            executed_at_ms: epoch_ms(),
            node_audit_root_hex: "cc".repeat(32),
        },
        operator,
    )
    .unwrap()
}

/// The lease fund shape, end to end: a buyer escrows a window's
/// ceiling, the session runs a fraction of it, and settlement releases
/// only the metered seconds while the rest goes back — the buyer is
/// charged for what ran, not for what was reserved. The meter is the
/// coordinator's own clock between accept and result; the operator's
/// receipt says the work happened but never sizes the bill.
#[tokio::test]
async fn a_lease_settles_on_metered_seconds_and_refunds_the_unused_window() {
    let (state, payout) = new_coordinator_state(Duration::from_secs(5));
    let state_handle = state.clone();
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state).await;

    // A lease-serving operator: priced by the GPU-hour, the only unit a
    // lease may use, and floored to the metered window before it is matched
    // against the envelope's escrowed price like any other job.
    let operator_identity = LocalIdentity::generate("operator@lease-e2e");
    let mut profile = cpu_profile(&operator_identity, 60_000);
    profile.job_kinds = vec![JobKind::LeaseSession];
    // A lease may be priced only by the GPU-hour; 360_000/hr is the per-hour
    // form of the buyer's 100 micro-USDC/s rate, so its metered-window floor
    // matches the envelope ceiling exactly, as the flat PerJob ask used to.
    profile.price = PriceAsk {
        unit: PriceUnit::PerLeaseHour,
        micro_usdc: 360_000,
    };
    let coordinator_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        2,
    ));
    let register_resp = coordinator_client
        .register(
            RegisterRequest::sign(profile.clone(), payout_addr(7), &operator_identity).unwrap(),
        )
        .await
        .unwrap();
    assert!(register_resp.accepted);

    // A session that actually occupies the machine for a beat: the
    // meter reads wall-clock time between accept and result, so an
    // instant executor would bill an honest zero and prove nothing.
    struct HeldSession;
    #[async_trait::async_trait]
    impl covenant_compute_node::JobExecutor for HeldSession {
        async fn execute(
            &self,
            _job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<covenant_compute_node::ExecutionOutcome, covenant_compute_node::ExecutorError>
        {
            tokio::time::sleep(Duration::from_millis(1_200)).await;
            Ok(covenant_compute_node::ExecutionOutcome {
                output: vec![Content::text("lease session ended")],
                wall_ms: 1_200,
                tokens_in: None,
                tokens_out: None,
                finish_reason: None,
            })
        }
    }

    let node = Node::new(
        operator_identity,
        profile,
        coordinator_client,
        Arc::new(HeldSession),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    );

    // 100 micro-USDC/s over a 600s window: the buyer signs — and
    // escrows — 60_000, the whole ceiling.
    let buyer_identity = LocalIdentity::generate("buyer@lease-e2e");
    let job_id = Uuid::new_v4();
    let envelope = signed_lease_envelope(&buyer_identity, job_id, 100, 600, epoch_ms(), "lease-1");
    assert_eq!(envelope.payload.price_micro_usdc, 60_000);

    let http = reqwest::Client::new();
    let submit = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit.status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Held,
        "the whole window is escrowed up front"
    );

    // The session runs and ends far inside its window.
    let outcome = node
        .run_once()
        .await
        .expect("run_once")
        .expect("the lease must be offered to the only lease-serving operator");
    assert_eq!(outcome.job_id, job_id);
    assert_eq!(outcome.receipt.receipt.status, A2ATaskStatus::Ok);

    let record = state_handle.jobs().get(job_id).unwrap();
    assert_eq!(
        record.phase,
        covenant_compute_coordinator::JobPhase::Completed
    );
    let elapsed_ms = record
        .metered_elapsed_ms
        .expect("a lease settlement pins the elapsed run it billed");
    assert!(
        (1_000..600_000).contains(&elapsed_ms),
        "the session ran for a beat and ended well inside its window, got {elapsed_ms}ms"
    );

    // What settled is the metered seconds, not the reserved ceiling.
    let terms = covenant_compute_protocol::parse_lease_terms(&record.envelope.payload.input)
        .unwrap()
        .unwrap();
    let expected = terms.metered_micro_usdc(elapsed_ms);
    assert!(
        (100..60_000).contains(&expected),
        "a short session costs its seconds, far under the whole window: {expected}"
    );
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released
    );
    let (settled_amount, _) = state_handle.escrow().hold_info(job_id).unwrap();
    assert_eq!(
        settled_amount, expected,
        "the hold is written down to the metered charge"
    );

    // The operator is paid the metered amount — never the ceiling.
    let records = payout.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].job_id, job_id);
    assert_eq!(records[0].amount_micro_usdc, expected);
    assert_eq!(records[0].payout_address, payout_addr(7));

    // And the buyer's books free the unused remainder: charged is the
    // metered figure, so the reserved-but-unused window is spendable
    // again with no second ledger row.
    let buyer_b58 = buyer_identity.agent_id().pubkey_base58();
    assert_eq!(
        state_handle.escrow().organic_charged(&buyer_b58),
        expected,
        "only the metered seconds stay charged against the buyer"
    );
}

/// The retry sweep sizes a lease payout from the seconds it ran, not the
/// window ceiling. A metered lease settles — its hold written down to the
/// used charge — but its first-chance push fails, so it lands on the retry
/// sweep released-but-unpaid. The retry must pay the metered net; paying
/// the whole escrowed window would overdraw the remainder already refunded
/// to the buyer and leave the coordinator short.
#[tokio::test]
async fn a_lease_payout_retry_pays_the_metered_seconds_not_the_window_ceiling() {
    let (state, payout) = new_coordinator_state(Duration::from_secs(5));
    // The first-chance push fails once, so the settled lease reaches the
    // retry sweep with its escrow released and no payout recorded.
    payout.fail_next_pays(1);
    let state_handle = state.clone();
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@lease-retry");
    let mut profile = cpu_profile(&operator_identity, 60_000);
    profile.job_kinds = vec![JobKind::LeaseSession];
    // A lease may be priced only by the GPU-hour; 360_000/hr is the per-hour
    // form of the buyer's 100 micro-USDC/s rate, so its metered-window floor
    // matches the envelope ceiling exactly, as the flat PerJob ask used to.
    profile.price = PriceAsk {
        unit: PriceUnit::PerLeaseHour,
        micro_usdc: 360_000,
    };
    let coordinator_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        2,
    ));
    assert!(
        coordinator_client
            .register(
                RegisterRequest::sign(profile.clone(), payout_addr(7), &operator_identity).unwrap(),
            )
            .await
            .unwrap()
            .accepted
    );

    struct HeldSession;
    #[async_trait::async_trait]
    impl covenant_compute_node::JobExecutor for HeldSession {
        async fn execute(
            &self,
            _job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<covenant_compute_node::ExecutionOutcome, covenant_compute_node::ExecutorError>
        {
            tokio::time::sleep(Duration::from_millis(1_200)).await;
            Ok(covenant_compute_node::ExecutionOutcome {
                output: vec![Content::text("lease session ended")],
                wall_ms: 1_200,
                tokens_in: None,
                tokens_out: None,
                finish_reason: None,
            })
        }
    }

    let node = Node::new(
        operator_identity,
        profile,
        coordinator_client,
        Arc::new(HeldSession),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    );

    // 100 micro-USDC/s over a 600s window: the buyer escrows the whole
    // 60_000 ceiling, but a short session owes far less.
    let buyer_identity = LocalIdentity::generate("buyer@lease-retry");
    let job_id = Uuid::new_v4();
    let envelope = signed_lease_envelope(
        &buyer_identity,
        job_id,
        100,
        600,
        epoch_ms(),
        "lease-retry-1",
    );
    assert_eq!(envelope.payload.price_micro_usdc, 60_000);

    let http = reqwest::Client::new();
    let submit = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit.status(), reqwest::StatusCode::ACCEPTED);

    node.run_once()
        .await
        .expect("run_once")
        .expect("the lease is offered to the only lease-serving operator");

    // Settled: escrow released and written down to the metered charge, but
    // the first-chance push failed, so nothing was paid.
    let record = state_handle.jobs().get(job_id).unwrap();
    assert_eq!(
        record.phase,
        covenant_compute_coordinator::JobPhase::Completed
    );
    assert!(
        record.payout.is_none(),
        "the first-chance push failed, so the job is owed on the sweep"
    );
    let terms = covenant_compute_protocol::parse_lease_terms(&record.envelope.payload.input)
        .unwrap()
        .unwrap();
    let expected = terms.metered_micro_usdc(record.metered_elapsed_ms.unwrap());
    assert!(
        (100..60_000).contains(&expected),
        "a short session bills well under the whole window: {expected}"
    );
    let (settled_amount, _) = state_handle.escrow().hold_info(job_id).unwrap();
    assert_eq!(
        settled_amount, expected,
        "the hold is written down to the metered charge"
    );
    assert!(
        payout.records().is_empty(),
        "the failed first push paid nothing"
    );

    // The retry pays the metered seconds — never the escrowed ceiling.
    let retried = sweep_unpaid(&state_handle).await;
    assert_eq!(retried, vec![job_id]);
    let records = payout.records();
    assert_eq!(records.len(), 1, "exactly one retry push lands");
    assert_eq!(
        records[0].amount_micro_usdc, expected,
        "the retry pays the metered draw, not the {} ceiling",
        envelope.payload.price_micro_usdc
    );
    assert!(
        records[0].amount_micro_usdc < 60_000,
        "the whole-window ceiling must never be paid for a short session"
    );
}

/// A lease nobody ever accepted has no meter to read: the coordinator
/// bills nothing and the buyer's whole escrowed window comes back, even
/// though a receipt arrived. The operator's own timestamps never fill
/// that gap — an unobserved session is worth zero.
#[tokio::test]
async fn a_lease_that_was_never_accepted_bills_nothing() {
    let (state, payout) = new_coordinator_state(Duration::from_secs(5));
    let base_url = spawn_coordinator(state.clone()).await;

    let operator_identity = LocalIdentity::generate("operator@lease-noaccept");
    let mut profile = cpu_profile(&operator_identity, 60_000);
    profile.job_kinds = vec![JobKind::LeaseSession];
    // A lease may be priced only by the GPU-hour; 360_000/hr is the per-hour
    // form of the buyer's 100 micro-USDC/s rate, so its metered-window floor
    // matches the envelope ceiling exactly, as the flat PerJob ask used to.
    profile.price = PriceAsk {
        unit: PriceUnit::PerLeaseHour,
        micro_usdc: 360_000,
    };
    let http = reqwest::Client::new();
    let session = register_session(
        &http,
        &base_url,
        &operator_identity,
        &profile,
        payout_addr(8),
    )
    .await;

    let buyer_identity = LocalIdentity::generate("buyer@lease-noaccept");
    let job_id = Uuid::new_v4();
    let envelope = signed_lease_envelope(&buyer_identity, job_id, 100, 600, epoch_ms(), "lease-2");
    http.post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();

    // Drain the offer so the job is assigned, then submit a result
    // WITHOUT ever accepting — the accept is what starts the meter.
    let offer: serde_json::Value = http
        .get(format!(
            "{base_url}/federation/operators/{}/next-job",
            operator_identity.agent_id().pubkey_base58()
        ))
        .header("authorization", format!("Bearer {session}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        offer
            .pointer("/envelope/payload/job_id")
            .and_then(|v| v.as_str()),
        Some(job_id.to_string().as_str()),
        "the lease is offered to the only lease-serving operator: {offer}"
    );

    let output = vec![Content::text("lease session ended")];
    let receipt = lease_receipt(job_id, &operator_identity, &envelope, &output);
    let ack = http
        .post(format!("{base_url}/federation/jobs/{job_id}/result"))
        .json(&serde_json::json!({"receipt": receipt, "output": output}))
        .send()
        .await
        .unwrap();
    assert_eq!(ack.status(), reqwest::StatusCode::OK);

    // Zero metered seconds settles as a refund: the buyer keeps the
    // whole window and nothing is paid out.
    assert_eq!(
        state.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Refunded,
        "an unobserved lease bills nothing"
    );
    assert_eq!(
        state
            .escrow()
            .organic_charged(&buyer_identity.agent_id().pubkey_base58()),
        0
    );
    assert!(payout.records().is_empty());
}

/// The whole product in one test: a buyer rents a machine, is told
/// where it is while it runs, watches the meter, ends the session early,
/// and is billed for the seconds it actually ran — with the rest of the
/// escrowed window returned. This is the loop a renter experiences, and
/// every number in it comes from the coordinator's own clock and the
/// buyer's own signed terms.
#[tokio::test]
async fn a_renter_gets_access_watches_the_meter_and_is_billed_for_what_they_used() {
    use covenant_compute_node::{LeaseControl, LeaseExecutor, StubSessionBackend};

    let (state, payout) = new_coordinator_state(Duration::from_secs(5));
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state.clone()).await;

    let operator_identity = LocalIdentity::generate("operator@rent");
    let mut profile = cpu_profile(&operator_identity, 60_000);
    profile.job_kinds = vec![JobKind::LeaseSession];
    // A lease may be priced only by the GPU-hour; 360_000/hr is the per-hour
    // form of the buyer's 100 micro-USDC/s rate, so its metered-window floor
    // matches the envelope ceiling exactly, as the flat PerJob ask used to.
    profile.price = PriceAsk {
        unit: PriceUnit::PerLeaseHour,
        micro_usdc: 360_000,
    };
    let client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        2,
    ));
    client
        .register(
            RegisterRequest::sign(profile.clone(), payout_addr(9), &operator_identity).unwrap(),
        )
        .await
        .unwrap();

    // The node serves sessions and watches the coordinator for the
    // buyer's close — the same wiring a real operator runs.
    let control = LeaseControl::new();
    let node = Node::new(
        operator_identity,
        profile,
        client.clone(),
        Arc::new(
            LeaseExecutor::new(
                Arc::new(StubSessionBackend::new("ssh renter@198.51.100.4 -p 2200")),
                control.clone(),
            )
            .watching(client.clone())
            .with_poll_interval(Duration::from_millis(50)),
        ),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    );

    // 100 micro-USDC/s for up to an hour: 360_000 escrowed up front.
    let buyer_identity = LocalIdentity::generate("buyer@rent");
    let job_id = Uuid::new_v4();
    let mut envelope =
        signed_lease_envelope(&buyer_identity, job_id, 100, 3_600, epoch_ms(), "rent-1");
    // Streaming is how the access grant reaches the buyer mid-session.
    let mut payload = envelope.payload.clone();
    payload.stream = true;
    envelope = SignedJobEnvelope::sign(payload, &buyer_identity).unwrap();

    let http = reqwest::Client::new();
    let submit = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit.status(), reqwest::StatusCode::ACCEPTED);

    // The session runs in the background while the buyer interacts.
    let serving = tokio::spawn(async move { node.run_once().await });

    // The buyer polls their lease and gets the address of the machine
    // they are paying for, without waiting for the session to end.
    let lease_path = format!("/federation/jobs/{job_id}/lease");
    let lease_url = format!("{base_url}{lease_path}");
    let mut access_endpoint = None;
    for _ in 0..100 {
        // The access grant names the operator's live machine, so it rides
        // back only to the lease's buyer: the buyer client signs this read,
        // and so does the test.
        let signed_at = epoch_ms();
        let sig =
            covenant_compute_protocol::sign_read(&buyer_identity, &lease_path, signed_at).unwrap();
        let view: serde_json::Value = http
            .get(&lease_url)
            .header(
                covenant_compute_protocol::READ_SIGNED_AT_HEADER,
                signed_at.to_string(),
            )
            .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, sig)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if let Some(endpoint) = view.pointer("/access/endpoint").and_then(|v| v.as_str()) {
            assert_eq!(view["status"], "accepted", "the session is live: {view}");
            access_endpoint = Some(endpoint.to_string());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        access_endpoint.as_deref(),
        Some("ssh renter@198.51.100.4 -p 2200"),
        "the renter is told where the machine is while the session runs"
    );

    // The endpoint rides back only to the buyer who signed the lease. The
    // serving node polls this same view unsigned for the close flag, so
    // status and the meter stay open — but an unsigned read never reveals
    // where the running machine is.
    let unsigned: serde_json::Value = http
        .get(&lease_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        unsigned["access"].is_null(),
        "an unsigned read never reveals where the machine is: {unsigned}"
    );
    assert_eq!(
        unsigned["status"], "accepted",
        "the node's unsigned close poll still reads status and the meter"
    );
    assert!(!unsigned["close_requested"].as_bool().unwrap());

    // A stranger who learns the job id cannot sign as its buyer, so a
    // forged read is refused outright rather than quietly downgraded.
    let stranger = LocalIdentity::generate("stranger@rent");
    let stranger_at = epoch_ms();
    let stranger_sig =
        covenant_compute_protocol::sign_read(&stranger, &lease_path, stranger_at).unwrap();
    let refused = http
        .get(&lease_url)
        .header(
            covenant_compute_protocol::READ_SIGNED_AT_HEADER,
            stranger_at.to_string(),
        )
        .header(
            covenant_compute_protocol::READ_SIGNATURE_HEADER,
            stranger_sig,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(
        refused.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "a read signed by anyone but the lease's buyer is refused"
    );

    // The meter is watchable and moving.
    tokio::time::sleep(Duration::from_millis(600)).await;
    let running: serde_json::Value = http
        .get(&lease_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(running["rate_micro_usdc_per_sec"], 100);
    assert_eq!(running["max_duration_secs"], 3_600);
    assert!(!running["close_requested"].as_bool().unwrap());
    let running_charge = running["charged_micro_usdc"].as_u64().unwrap();
    assert!(
        running_charge > 0 && running_charge < 360_000,
        "a live meter shows a partial charge, got {running_charge}"
    );

    // The renter is done and ends the session.
    let close = covenant_compute_protocol::LeaseCloseRequest::sign(
        buyer_identity.agent_id(),
        job_id,
        epoch_ms(),
        &buyer_identity,
    )
    .unwrap();
    let closed: serde_json::Value = http
        .post(format!("{base_url}/federation/jobs/{job_id}/close"))
        .json(&close)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(closed["close_requested"].as_bool().unwrap());

    // The node sees the close, releases the machine and submits its
    // receipt, which settles the meter.
    let outcome = serving
        .await
        .unwrap()
        .expect("run_once")
        .expect("the lease was offered to this operator");
    assert_eq!(outcome.job_id, job_id);
    assert_eq!(outcome.receipt.receipt.status, A2ATaskStatus::Ok);

    let record = state.jobs().get(job_id).unwrap();
    assert_eq!(
        record.phase,
        covenant_compute_coordinator::JobPhase::Completed
    );
    let billed_ms = record.metered_elapsed_ms.unwrap();
    assert!(
        billed_ms < 3_600_000,
        "the session ended on the buyer's close, far inside its window: {billed_ms}ms"
    );

    // Charged for the seconds served, refunded the rest of the window.
    let terms = covenant_compute_protocol::parse_lease_terms(&record.envelope.payload.input)
        .unwrap()
        .unwrap();
    let charged = terms.metered_micro_usdc(billed_ms);
    let (settled, _) = state.escrow().hold_info(job_id).unwrap();
    assert_eq!(settled, charged);
    assert!(
        charged < 3_600,
        "a sub-minute session costs well under a minute of the window: {charged}"
    );
    assert_eq!(payout.records()[0].amount_micro_usdc, charged);
    assert_eq!(
        state
            .escrow()
            .organic_charged(&buyer_identity.agent_id().pubkey_base58()),
        charged,
        "the unused window is the buyer's again"
    );

    // The final view is self-consistent: what the buyer watched is what
    // they were billed.
    let final_view: serde_json::Value = http
        .get(&lease_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(final_view["status"], "completed");
    assert_eq!(final_view["charged_micro_usdc"].as_u64().unwrap(), charged);
    assert_eq!(final_view["elapsed_ms"].as_u64().unwrap(), billed_ms);

    // And the close is on the audit chain as its own fact.
    let events = state.audit().recent(usize::MAX).await.unwrap();
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        AuditKind::ComputeLeaseCloseRequested { job_id: id, .. } if *id == job_id
    )));
}

/// The on-chain lease meter's seam, driven end to end against the
/// recording meter: a real lease runs through the ordinary pipeline
/// with `CoordinatorConfig::lease_meter` set, and every off-chain
/// outcome — escrow, payout, buyer books — comes out identical to the
/// unmetered run above. The meter itself sees the whole lifecycle: one
/// open at accept, ticks while the session runs, one conclusion on
/// exactly the elapsed the record pins as the charge's explanation.
///
/// No network: the recording meter moves nothing and signs nothing,
/// which is also why the off-chain payout push stays in charge.
#[tokio::test]
async fn a_lease_metered_on_chain_settles_exactly_as_an_unmetered_one() {
    let meter = Arc::new(covenant_compute_coordinator::NoopLeaseMeter::new());
    let identity = LocalIdentity::generate("coordinator@lease-meter-e2e");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    let state = CoordinatorState::new(
        identity,
        CoordinatorConfig {
            long_poll_timeout: Duration::from_secs(5),
            default_funding_source: FundingSource::Organic,
            lease_meter: Some(meter.clone()),
            ..CoordinatorConfig::default()
        },
        reputation,
        payout.clone(),
        audit,
    );
    let state_handle = state.clone();
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@lease-meter-e2e");
    let mut profile = cpu_profile(&operator_identity, 60_000);
    profile.job_kinds = vec![JobKind::LeaseSession];
    // A lease may be priced only by the GPU-hour; 360_000/hr is the per-hour
    // form of the buyer's 100 micro-USDC/s rate, so its metered-window floor
    // matches the envelope ceiling exactly, as the flat PerJob ask used to.
    profile.price = PriceAsk {
        unit: PriceUnit::PerLeaseHour,
        micro_usdc: 360_000,
    };
    let coordinator_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        2,
    ));
    assert!(
        coordinator_client
            .register(
                RegisterRequest::sign(profile.clone(), payout_addr(21), &operator_identity)
                    .unwrap(),
            )
            .await
            .unwrap()
            .accepted
    );

    // The session has to occupy the machine long enough for the tick
    // pass to see it running; an instant executor would prove only the
    // open and the conclusion.
    struct HeldSession;
    #[async_trait::async_trait]
    impl covenant_compute_node::JobExecutor for HeldSession {
        async fn execute(
            &self,
            _job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<covenant_compute_node::ExecutionOutcome, covenant_compute_node::ExecutorError>
        {
            tokio::time::sleep(Duration::from_millis(1_200)).await;
            Ok(covenant_compute_node::ExecutionOutcome {
                output: vec![Content::text("lease session ended")],
                wall_ms: 1_200,
                tokens_in: None,
                tokens_out: None,
                finish_reason: None,
            })
        }
    }

    let node = Node::new(
        operator_identity,
        profile,
        coordinator_client,
        Arc::new(HeldSession),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    );

    let buyer_identity = LocalIdentity::generate("buyer@lease-meter-e2e");
    let job_id = Uuid::new_v4();
    let envelope = signed_lease_envelope(
        &buyer_identity,
        job_id,
        100,
        600,
        epoch_ms(),
        "lease-meter-1",
    );
    assert_eq!(envelope.payload.price_micro_usdc, 60_000);

    let http = reqwest::Client::new();
    let submit = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit.status(), reqwest::StatusCode::ACCEPTED);
    assert!(
        meter.opened().is_empty(),
        "an offered-but-unaccepted lease opens no vault — nobody is serving it yet"
    );

    // The real periodic pass, at a test cadence, for the length of the
    // session.
    let ticker = covenant_compute_coordinator::spawn_periodic_lease_meter(
        state_handle.clone(),
        Duration::from_millis(100),
    );
    let outcome = node
        .run_once()
        .await
        .expect("run_once")
        .expect("the lease must be offered to the only lease-serving operator");
    ticker.abort();
    assert_eq!(outcome.job_id, job_id);
    assert_eq!(outcome.receipt.receipt.status, A2ATaskStatus::Ok);

    let record = state_handle.jobs().get(job_id).unwrap();
    assert_eq!(
        record.phase,
        covenant_compute_coordinator::JobPhase::Completed
    );
    let elapsed_ms = record.metered_elapsed_ms.unwrap();

    // One open, at accept, on the buyer's signed terms.
    let opened = meter.opened();
    assert_eq!(opened.len(), 1, "one accept, one vault");
    assert_eq!(opened[0].job_id, job_id);
    assert_eq!(opened[0].rate_micro_usdc_per_sec, 100);
    assert_eq!(opened[0].max_duration_secs, 600);
    assert_eq!(
        opened[0].funded_micro_usdc(),
        envelope.payload.price_micro_usdc,
        "the vault is funded with the same ceiling the buyer escrowed"
    );
    assert_eq!(
        opened[0].accepted_at_ms,
        record.accepted_at_ms.unwrap(),
        "both meters start from one timestamp"
    );
    assert_eq!(opened[0].operator_payout_address, payout_addr(21));

    // Ticks while it ran, each one cumulative and non-decreasing.
    let ticks = meter.ticks();
    assert!(
        !ticks.is_empty(),
        "a session held for over a second must have been ticked at a 100ms cadence"
    );
    for pair in ticks.windows(2) {
        assert!(
            pair[1].elapsed_ms >= pair[0].elapsed_ms,
            "the meter only ever moves forward"
        );
    }
    for tick in &ticks {
        assert_eq!(tick.job_id, job_id);
        assert_eq!(
            tick.charged_micro_usdc,
            covenant_compute_protocol::parse_lease_terms(&record.envelope.payload.input)
                .unwrap()
                .unwrap()
                .metered_micro_usdc(tick.elapsed_ms),
            "a tick charges what the buyer's own signed terms say"
        );
        assert!(
            tick.elapsed_ms <= elapsed_ms,
            "no tick may overshoot the elapsed the record finally billed"
        );
    }
    assert_ne!(
        ticks[0].receipt_hash(),
        ticks[ticks.len() - 1].receipt_hash(),
        "distinct observations fold distinct hashes into the provenance chain"
    );

    // One conclusion, on exactly the figure the record pins.
    let concluded = meter.concluded();
    assert_eq!(concluded.len(), 1);
    assert_eq!(concluded[0].elapsed_ms, elapsed_ms);
    let terms = covenant_compute_protocol::parse_lease_terms(&record.envelope.payload.input)
        .unwrap()
        .unwrap();
    let expected = terms.metered_micro_usdc(elapsed_ms);
    assert_eq!(
        concluded[0].charged_micro_usdc, expected,
        "the on-chain charge and the off-chain charge are one number"
    );

    // And the money is exactly where the unmetered run leaves it.
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Released
    );
    assert_eq!(state_handle.escrow().hold_info(job_id).unwrap().0, expected);
    let records = payout.records();
    assert_eq!(
        records.len(),
        1,
        "a meter that moved no money leaves the off-chain payout push in charge"
    );
    assert_eq!(records[0].amount_micro_usdc, expected);
    assert_eq!(records[0].payout_address, payout_addr(21));
    assert_eq!(
        state_handle
            .escrow()
            .organic_charged(&buyer_identity.agent_id().pubkey_base58()),
        expected
    );
    assert!(
        state_handle.jobs().completed_unpaid().is_empty(),
        "the job's payout is recorded, so the retry sweep has nothing to spin on"
    );
    assert!(
        state_handle.jobs().live_leases().is_empty(),
        "a concluded lease leaves the tick worklist"
    );
}

/// A settle whose outcome the signer could not see. The vault may already
/// have paid the operator, so pushing too could pay twice: the payout is
/// held in a suspended transfer bracket that neither the push nor the
/// retry sweep may touch until an admin reads the chain.
#[tokio::test]
async fn a_lease_settle_of_unknown_outcome_holds_the_payout() {
    use covenant_compute_coordinator::{
        LeaseMeter, LeaseMeterError, LeaseObservation, LeaseOpen, LeaseSettlement, NoopLeaseMeter,
    };

    struct UnknownSettle(NoopLeaseMeter);

    #[async_trait::async_trait]
    impl LeaseMeter for UnknownSettle {
        async fn open_and_delegate(
            &self,
            open: &LeaseOpen,
        ) -> Result<Option<String>, LeaseMeterError> {
            self.0.open_and_delegate(open).await
        }
        async fn tick(&self, observation: &LeaseObservation) -> Result<bool, LeaseMeterError> {
            self.0.tick(observation).await
        }
        async fn undelegate_and_settle(
            &self,
            _observation: &LeaseObservation,
            _payout_memo: Option<&str>,
        ) -> Result<Option<LeaseSettlement>, LeaseMeterError> {
            Err(LeaseMeterError::Unresolved {
                message: "settle_lease: not confirmed within 60s".into(),
                tx_signature: Some("settle-signature".into()),
            })
        }
        async fn void(&self, job_id: Uuid) -> Result<Option<LeaseSettlement>, LeaseMeterError> {
            self.0.void(job_id).await
        }
        fn adopt(&self, open: &LeaseOpen) {
            self.0.adopt(open)
        }
        fn describe(&self) -> String {
            "unknown settle".into()
        }
    }

    let identity = LocalIdentity::generate("coordinator@lease-meter-unknown-settle");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    let state = CoordinatorState::new(
        identity,
        CoordinatorConfig {
            long_poll_timeout: Duration::from_secs(5),
            default_funding_source: FundingSource::Organic,
            lease_meter: Some(Arc::new(UnknownSettle(NoopLeaseMeter::new()))),
            ..CoordinatorConfig::default()
        },
        reputation,
        payout.clone(),
        audit,
    );
    let state_handle = state.clone();
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@lease-meter-unknown-settle");
    let mut profile = cpu_profile(&operator_identity, 60_000);
    profile.job_kinds = vec![JobKind::LeaseSession];
    profile.price = PriceAsk {
        unit: PriceUnit::PerLeaseHour,
        micro_usdc: 360_000,
    };
    let coordinator_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        2,
    ));
    assert!(
        coordinator_client
            .register(
                RegisterRequest::sign(profile.clone(), payout_addr(22), &operator_identity)
                    .unwrap(),
            )
            .await
            .unwrap()
            .accepted
    );

    struct HeldSession;
    #[async_trait::async_trait]
    impl covenant_compute_node::JobExecutor for HeldSession {
        async fn execute(
            &self,
            _job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<covenant_compute_node::ExecutionOutcome, covenant_compute_node::ExecutorError>
        {
            tokio::time::sleep(Duration::from_millis(300)).await;
            Ok(covenant_compute_node::ExecutionOutcome {
                output: vec![Content::text("lease session ended")],
                wall_ms: 300,
                tokens_in: None,
                tokens_out: None,
                finish_reason: None,
            })
        }
    }

    let node = Node::new(
        operator_identity,
        profile,
        coordinator_client,
        Arc::new(HeldSession),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    );

    let buyer_identity = LocalIdentity::generate("buyer@lease-meter-unknown-settle");
    let job_id = Uuid::new_v4();
    let envelope = signed_lease_envelope(
        &buyer_identity,
        job_id,
        100,
        600,
        epoch_ms(),
        "lease-meter-unknown-settle",
    );
    let submit = reqwest::Client::new()
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit.status(), reqwest::StatusCode::ACCEPTED);

    let outcome = node
        .run_once()
        .await
        .expect("run_once")
        .expect("the lease must be offered to the only lease-serving operator");
    assert_eq!(outcome.job_id, job_id);

    let record = state_handle.jobs().get(job_id).unwrap();
    assert_eq!(
        record.phase,
        covenant_compute_coordinator::JobPhase::Completed,
        "the operator's result is accepted; only the payout waits"
    );
    assert!(
        payout.records().is_empty(),
        "a vault that may have paid must not be paid over"
    );
    let held = state_handle
        .attempts()
        .get(job_id)
        .expect("the payout is held in an open transfer bracket");
    assert_eq!(held.tx_signature.as_deref(), Some("settle-signature"));
    assert_eq!(held.recipient_address_b58, payout_addr(22));
    assert!(held.detail.contains("not confirmed"), "{}", held.detail);

    assert!(
        sweep_unpaid(&state_handle).await.is_empty(),
        "the retry sweep leaves a held payout alone"
    );
    assert!(payout.records().is_empty());
}

/// The coordinator driving the real lease signer on Solana devnet: the
/// accept opens and delegates the lease, the tick pass meters it in the
/// rollup, and the result settles it out of the vault, which becomes the
/// job's payout of record. A settle takes longer than the node's call
/// timeout, so this also runs the retried result through the conclusion
/// still in flight.
///
/// Needs the built signer, a renter funded in SOL and holding the escrow
/// mint, and a separate coordinator key; `lease-signer-e2e.mjs` sets both
/// up on devnet. Every `LEASE_E2E_*` variable below is required.
#[tokio::test]
#[ignore = "live devnet: needs the built lease signer and a funded renter"]
async fn a_lease_settles_on_devnet_through_the_signer() {
    use covenant_compute_coordinator::{SidecarLeaseMeter, SidecarLeaseMeterConfig};

    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is required"));
    let meter = Arc::new(SidecarLeaseMeter::new(SidecarLeaseMeterConfig {
        signer_binary: var("LEASE_E2E_SIGNER").into(),
        program_id: var("LEASE_E2E_PROGRAM"),
        mint: var("LEASE_E2E_MINT"),
        rpc_url: var("LEASE_E2E_RPC"),
        er_rpc_url: var("LEASE_E2E_ER"),
        er_validator: var("LEASE_E2E_VALIDATOR"),
        renter_keypair_path: var("LEASE_E2E_RENTER"),
        coordinator_keypair_path: var("LEASE_E2E_COORDINATOR"),
    }));
    let identity = LocalIdentity::generate("coordinator@lease-signer-devnet");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    let state = CoordinatorState::new(
        identity,
        CoordinatorConfig {
            long_poll_timeout: Duration::from_secs(5),
            default_funding_source: FundingSource::Organic,
            lease_meter: Some(meter.clone()),
            ..CoordinatorConfig::default()
        },
        reputation,
        payout.clone(),
        audit,
    );
    let state_handle = state.clone();
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@lease-signer-devnet");
    let mut profile = cpu_profile(&operator_identity, 60_000);
    profile.job_kinds = vec![JobKind::LeaseSession];
    profile.price = PriceAsk {
        unit: PriceUnit::PerLeaseHour,
        micro_usdc: 360_000,
    };
    // A node keeps a result until the coordinator acknowledges it; enough
    // retries here stand in for that outbox.
    let coordinator_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        30,
    ));
    assert!(
        coordinator_client
            .register(
                RegisterRequest::sign(profile.clone(), payout_addr(24), &operator_identity)
                    .unwrap(),
            )
            .await
            .unwrap()
            .accepted
    );

    struct HeldSession;
    #[async_trait::async_trait]
    impl covenant_compute_node::JobExecutor for HeldSession {
        async fn execute(
            &self,
            _job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<covenant_compute_node::ExecutionOutcome, covenant_compute_node::ExecutorError>
        {
            tokio::time::sleep(Duration::from_secs(8)).await;
            Ok(covenant_compute_node::ExecutionOutcome {
                output: vec![Content::text("lease session ended")],
                wall_ms: 8_000,
                tokens_in: None,
                tokens_out: None,
                finish_reason: None,
            })
        }
    }

    let node = Node::new(
        operator_identity,
        profile,
        coordinator_client,
        Arc::new(HeldSession),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    );

    let buyer_identity = LocalIdentity::generate("buyer@lease-signer-devnet");
    let job_id = Uuid::new_v4();
    let envelope = signed_lease_envelope(
        &buyer_identity,
        job_id,
        100,
        600,
        epoch_ms(),
        "lease-signer-devnet",
    );
    let submit = reqwest::Client::new()
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit.status(), reqwest::StatusCode::ACCEPTED);

    let ticker = covenant_compute_coordinator::spawn_periodic_lease_meter(
        state_handle.clone(),
        Duration::from_secs(2),
    );
    let outcome = node
        .run_once()
        .await
        .expect("run_once")
        .expect("the lease must be offered to the only lease-serving operator");
    ticker.abort();
    assert_eq!(outcome.job_id, job_id);

    let record = state_handle.jobs().get(job_id).unwrap();
    assert_eq!(
        record.phase,
        covenant_compute_coordinator::JobPhase::Completed
    );
    let settled = meter
        .settlement_for(job_id)
        .expect("the lease settled on-chain");
    let settle_tx = settled.tx_signature.clone().expect("with a transaction");
    assert_eq!(settled.metered_ms, record.metered_elapsed_ms.unwrap());
    let recorded = record.payout.expect("the job has a payout of record");
    assert_eq!(
        recorded.tx_signature.as_deref(),
        Some(settle_tx.as_str()),
        "the vault's settle is the payout"
    );
    assert_eq!(recorded.amount_micro_usdc, settled.charged_micro_usdc);
    assert!(
        payout.records().is_empty(),
        "an operator paid from the vault is not paid again"
    );

    // The buyer's `verify` and the operator's `earnings verify` read a
    // payout off the chain the same way; the vault's payment has to pass it.
    let receipt = record.receipt.as_ref().expect("the verified receipt");
    let request = covenant_compute_protocol::payout_transaction_rpc_request(&settle_tx);
    let mut tx = serde_json::Value::Null;
    for _ in 0..10 {
        let body: serde_json::Value = reqwest::Client::new()
            .post(var("LEASE_E2E_RPC"))
            .json(&request)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        tx = body["result"].clone();
        if !tx.is_null() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let proof = covenant_compute_protocol::verify_payout_transaction(&receipt.payout_memo(), &tx)
        .expect("the vault's payment verifies like any payout");
    assert_eq!(proof.amount_micro_usdc, settled.charged_micro_usdc);
    assert_eq!(proof.recipient_owner_b58, payout_addr(24));
    assert_eq!(proof.mint_b58, var("LEASE_E2E_MINT"));
    eprintln!(
        "job {job_id} paid from the vault in {settle_tx}: {} ms, {} micro-units, verified",
        settled.metered_ms, settled.charged_micro_usdc
    );
}

/// A hold refunded while the session was still running — the deadline
/// sweep got there first — must never be concluded on-chain. The escrow
/// is released before the meter settles, so a refunded job fails the
/// release and the vault is never asked to pay for work the books did
/// not bill.
#[tokio::test]
async fn a_lease_refunded_mid_session_never_settles_on_chain() {
    let meter = Arc::new(covenant_compute_coordinator::NoopLeaseMeter::new());
    let identity = LocalIdentity::generate("coordinator@lease-meter-refunded");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    let state = CoordinatorState::new(
        identity,
        CoordinatorConfig {
            long_poll_timeout: Duration::from_secs(5),
            default_funding_source: FundingSource::Organic,
            lease_meter: Some(meter.clone()),
            ..CoordinatorConfig::default()
        },
        reputation,
        payout.clone(),
        audit,
    );
    let state_handle = state.clone();
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@lease-meter-refunded");
    let mut profile = cpu_profile(&operator_identity, 60_000);
    profile.job_kinds = vec![JobKind::LeaseSession];
    profile.price = PriceAsk {
        unit: PriceUnit::PerLeaseHour,
        micro_usdc: 360_000,
    };
    let coordinator_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        2,
    ));
    assert!(
        coordinator_client
            .register(
                RegisterRequest::sign(profile.clone(), payout_addr(23), &operator_identity)
                    .unwrap(),
            )
            .await
            .unwrap()
            .accepted
    );

    struct HeldSession;
    #[async_trait::async_trait]
    impl covenant_compute_node::JobExecutor for HeldSession {
        async fn execute(
            &self,
            _job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<covenant_compute_node::ExecutionOutcome, covenant_compute_node::ExecutorError>
        {
            tokio::time::sleep(Duration::from_millis(1_000)).await;
            Ok(covenant_compute_node::ExecutionOutcome {
                output: vec![Content::text("lease session ended")],
                wall_ms: 1_000,
                tokens_in: None,
                tokens_out: None,
                finish_reason: None,
            })
        }
    }

    let node = Node::new(
        operator_identity,
        profile,
        coordinator_client,
        Arc::new(HeldSession),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    );

    let buyer_identity = LocalIdentity::generate("buyer@lease-meter-refunded");
    let job_id = Uuid::new_v4();
    let envelope = signed_lease_envelope(
        &buyer_identity,
        job_id,
        100,
        600,
        epoch_ms(),
        "lease-meter-refunded",
    );
    let submit = reqwest::Client::new()
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit.status(), reqwest::StatusCode::ACCEPTED);

    // The sweep's refund, landing while the session is still held.
    let refunder = {
        let state = state_handle.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            state
                .escrow()
                .refund(
                    job_id,
                    covenant_compute_protocol::RefundReason::DeadlineExpired,
                )
                .await
        })
    };
    let _ = node.run_once().await;
    refunder.await.unwrap().expect("the refund lands first");

    assert_eq!(meter.opened().len(), 1, "the accept opened the vault");
    assert!(
        meter.concluded().is_empty(),
        "a refunded hold must not be settled on-chain"
    );
    assert!(payout.records().is_empty(), "and nothing is pushed");
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Refunded
    );
}

/// A lease that concluded without ever being accepted has no meter to
/// read: the on-chain seam opens nothing, so there is no vault to
/// strand, and the buyer's whole window still comes back.
#[tokio::test]
async fn an_unaccepted_lease_opens_no_vault() {
    let meter = Arc::new(covenant_compute_coordinator::NoopLeaseMeter::new());
    let identity = LocalIdentity::generate("coordinator@lease-meter-unaccepted");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    let state = CoordinatorState::new(
        identity,
        CoordinatorConfig {
            long_poll_timeout: Duration::from_secs(5),
            default_funding_source: FundingSource::Organic,
            lease_meter: Some(meter.clone()),
            ..CoordinatorConfig::default()
        },
        reputation,
        payout.clone(),
        audit,
    );

    let operator_identity = LocalIdentity::generate("operator@lease-meter-unaccepted");
    let mut profile = cpu_profile(&operator_identity, 60_000);
    profile.job_kinds = vec![JobKind::LeaseSession];
    // A lease may be priced only by the GPU-hour; 360_000/hr is the per-hour
    // form of the buyer's 100 micro-USDC/s rate, so its metered-window floor
    // matches the envelope ceiling exactly, as the flat PerJob ask used to.
    profile.price = PriceAsk {
        unit: PriceUnit::PerLeaseHour,
        micro_usdc: 360_000,
    };
    let base_url = spawn_coordinator(state.clone()).await;
    let http = reqwest::Client::new();
    register_session(
        &http,
        &base_url,
        &operator_identity,
        &profile,
        payout_addr(22),
    )
    .await;

    let buyer_identity = LocalIdentity::generate("buyer@lease-meter-unaccepted");
    let job_id = Uuid::new_v4();
    let envelope = signed_lease_envelope(
        &buyer_identity,
        job_id,
        100,
        600,
        epoch_ms(),
        "lease-meter-2",
    );
    let submit = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit.status(), reqwest::StatusCode::ACCEPTED);

    assert_eq!(
        covenant_compute_coordinator::tick_live_leases(&state, epoch_ms()).await,
        0,
        "an offered lease has no t0, so it is not on the tick worklist"
    );
    assert!(meter.opened().is_empty());
    assert!(meter.ticks().is_empty());
    assert!(meter.concluded().is_empty());
    assert_eq!(
        state.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Held
    );
}

/// A lease whose session fails is refunded in full off-chain, and the
/// on-chain vault has to agree: an operator that ran metered seconds
/// before crashing must not be able to settle the funded vault for them.
/// The failed-receipt path voids the lease — charge zero, the whole
/// window home — instead of concluding it on the elapsed.
#[tokio::test]
async fn a_failed_lease_receipt_voids_the_on_chain_vault() {
    let meter = Arc::new(covenant_compute_coordinator::NoopLeaseMeter::new());
    let identity = LocalIdentity::generate("coordinator@lease-void-fail");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    let state = CoordinatorState::new(
        identity,
        CoordinatorConfig {
            long_poll_timeout: Duration::from_secs(5),
            default_funding_source: FundingSource::Organic,
            lease_meter: Some(meter.clone()),
            ..CoordinatorConfig::default()
        },
        reputation,
        payout.clone(),
        audit,
    );
    let state_handle = state.clone();
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@lease-void-fail");
    let mut profile = cpu_profile(&operator_identity, 60_000);
    profile.job_kinds = vec![JobKind::LeaseSession];
    // A lease may be priced only by the GPU-hour; 360_000/hr is the per-hour
    // form of the buyer's 100 micro-USDC/s rate, so its metered-window floor
    // matches the envelope ceiling exactly, as the flat PerJob ask used to.
    profile.price = PriceAsk {
        unit: PriceUnit::PerLeaseHour,
        micro_usdc: 360_000,
    };
    let coordinator_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.clone(),
        Duration::from_secs(5),
        2,
    ));
    assert!(
        coordinator_client
            .register(
                RegisterRequest::sign(profile.clone(), payout_addr(24), &operator_identity)
                    .unwrap(),
            )
            .await
            .unwrap()
            .accepted
    );

    struct FailingSession;
    #[async_trait::async_trait]
    impl covenant_compute_node::JobExecutor for FailingSession {
        async fn execute(
            &self,
            _job: &JobEnvelopePayload,
            _deadline: Duration,
        ) -> Result<covenant_compute_node::ExecutionOutcome, covenant_compute_node::ExecutorError>
        {
            Err(covenant_compute_node::ExecutorError::Failed(
                "session backend crashed".into(),
            ))
        }
    }

    let node = Node::new(
        operator_identity,
        profile,
        coordinator_client,
        Arc::new(FailingSession),
        Arc::new(InMemoryEarningsLedger::new()),
        Arc::new(InMemoryAuditLog::new()),
        NodeConfig {
            coordinator_pubkey_b58,
            max_in_flight: 4,
            preempt_grace: Duration::from_secs(2),
            fee_bps: 0,
        },
    );

    let buyer_identity = LocalIdentity::generate("buyer@lease-void-fail");
    let job_id = Uuid::new_v4();
    let envelope = signed_lease_envelope(
        &buyer_identity,
        job_id,
        100,
        600,
        epoch_ms(),
        "lease-void-fail",
    );

    let http = reqwest::Client::new();
    let submit = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit.status(), reqwest::StatusCode::ACCEPTED);

    let outcome = node
        .run_once()
        .await
        .expect("run_once")
        .expect("the lease is offered to the only lease-serving operator");
    assert_eq!(outcome.job_id, job_id);
    assert_eq!(
        outcome.receipt.receipt.status,
        A2ATaskStatus::Error,
        "a crashed session reports a failed receipt"
    );

    // The accept opened the vault; the failure voids it rather than
    // concluding it, so the operator is paid on neither book.
    let opened = meter.opened();
    assert_eq!(opened.len(), 1, "the accept opened one vault");
    assert_eq!(opened[0].job_id, job_id);
    assert_eq!(
        meter.voided(),
        vec![job_id],
        "the failed receipt voids the vault whole to the renter"
    );
    assert!(
        meter.concluded().is_empty(),
        "a void is not a settlement — nothing pays the operator on-chain"
    );
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Refunded
    );
    assert!(payout.records().is_empty(), "a failed lease pays no one");
    assert_eq!(
        state_handle.jobs().get(job_id).unwrap().phase,
        covenant_compute_coordinator::JobPhase::Failed
    );

    // The buyer's lease view must show the whole refund, not a meter that
    // keeps climbing after the money came back. A failed lease was
    // accepted, so its accept timestamp is set, but it settled without a
    // metered stamp, so it bills nothing however long ago that accept was.
    let view: serde_json::Value = http
        .get(format!("{base_url}/federation/jobs/{job_id}/lease"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(view["status"], "failed");
    assert_eq!(view["elapsed_ms"].as_u64().unwrap(), 0);
    assert_eq!(
        view["charged_micro_usdc"].as_u64().unwrap(),
        0,
        "a refunded lease bills nothing, whatever the wall clock reads"
    );
}

/// A lease that is accepted but never concludes — the session stalls and
/// the deadline sweep reclaims it — is refunded in full, and the vault is
/// voided so the operator cannot later settle it for the window the buyer
/// got back.
#[tokio::test]
async fn an_expired_accepted_lease_voids_the_on_chain_vault() {
    let meter = Arc::new(covenant_compute_coordinator::NoopLeaseMeter::new());
    let identity = LocalIdentity::generate("coordinator@lease-void-expire");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    let state = CoordinatorState::new(
        identity,
        CoordinatorConfig {
            long_poll_timeout: Duration::from_secs(5),
            default_funding_source: FundingSource::Organic,
            lease_meter: Some(meter.clone()),
            ..CoordinatorConfig::default()
        },
        reputation,
        payout.clone(),
        audit,
    );
    let state_handle = state.clone();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@lease-void-expire");
    let mut profile = cpu_profile(&operator_identity, 60_000);
    profile.job_kinds = vec![JobKind::LeaseSession];
    // A lease may be priced only by the GPU-hour; 360_000/hr is the per-hour
    // form of the buyer's 100 micro-USDC/s rate, so its metered-window floor
    // matches the envelope ceiling exactly, as the flat PerJob ask used to.
    profile.price = PriceAsk {
        unit: PriceUnit::PerLeaseHour,
        micro_usdc: 360_000,
    };
    let http = reqwest::Client::new();
    let session = register_session(
        &http,
        &base_url,
        &operator_identity,
        &profile,
        payout_addr(25),
    )
    .await;

    let buyer_identity = LocalIdentity::generate("buyer@lease-void-expire");
    let job_id = Uuid::new_v4();
    let issued_at_ms = epoch_ms();
    let envelope = signed_lease_envelope(
        &buyer_identity,
        job_id,
        100,
        600,
        issued_at_ms,
        "lease-void-expire",
    );
    let deadline_ms = envelope.payload.deadline_ms;

    let submit = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit.status(), reqwest::StatusCode::ACCEPTED);

    // The operator accepts, which opens the on-chain vault, then the
    // session never reports a result.
    let accept = http
        .post(format!("{base_url}/federation/jobs/{job_id}/accept"))
        .bearer_auth(&session)
        .json(&covenant_compute_protocol::JobAccept::Accept { job_id })
        .send()
        .await
        .unwrap();
    assert_eq!(accept.status(), reqwest::StatusCode::OK);
    let opened = meter.opened();
    assert_eq!(opened.len(), 1, "the accept opened one vault");
    assert_eq!(opened[0].job_id, job_id);

    // The deadline lapses with no result: the sweep refunds the buyer and
    // voids the vault so nothing is left for the operator to settle.
    let refunded = sweep_expired(&state_handle, issued_at_ms + deadline_ms + 1).await;
    assert_eq!(refunded, vec![job_id]);
    assert_eq!(
        meter.voided(),
        vec![job_id],
        "the expiry sweep voids the vault"
    );
    assert!(
        meter.concluded().is_empty(),
        "an expired lease is voided, never settled"
    );
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Refunded
    );
    assert!(payout.records().is_empty());
    assert_eq!(
        state_handle.jobs().get(job_id).unwrap().phase,
        covenant_compute_coordinator::JobPhase::Refunded
    );
}

/// A lease the operator accepts and then rejects is refunded in full off
/// chain, and the vault opened at that accept has to be voided too —
/// otherwise the operator could still settle it for the seconds between
/// its own accept and its reject, the very seconds the buyer was refunded.
#[tokio::test]
async fn a_lease_rejected_after_accept_voids_the_on_chain_vault() {
    let meter = Arc::new(covenant_compute_coordinator::NoopLeaseMeter::new());
    let identity = LocalIdentity::generate("coordinator@lease-void-reject");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    let state = CoordinatorState::new(
        identity,
        CoordinatorConfig {
            long_poll_timeout: Duration::from_secs(5),
            default_funding_source: FundingSource::Organic,
            lease_meter: Some(meter.clone()),
            ..CoordinatorConfig::default()
        },
        reputation,
        payout.clone(),
        audit,
    );
    let state_handle = state.clone();
    let base_url = spawn_coordinator(state).await;

    let operator_identity = LocalIdentity::generate("operator@lease-void-reject");
    let mut profile = cpu_profile(&operator_identity, 60_000);
    profile.job_kinds = vec![JobKind::LeaseSession];
    // A lease may be priced only by the GPU-hour; 360_000/hr is the per-hour
    // form of the buyer's 100 micro-USDC/s rate, so its metered-window floor
    // matches the envelope ceiling exactly, as the flat PerJob ask used to.
    profile.price = PriceAsk {
        unit: PriceUnit::PerLeaseHour,
        micro_usdc: 360_000,
    };
    let http = reqwest::Client::new();
    let session = register_session(
        &http,
        &base_url,
        &operator_identity,
        &profile,
        payout_addr(26),
    )
    .await;

    let buyer_identity = LocalIdentity::generate("buyer@lease-void-reject");
    let job_id = Uuid::new_v4();
    let envelope = signed_lease_envelope(
        &buyer_identity,
        job_id,
        100,
        600,
        epoch_ms(),
        "lease-void-reject",
    );
    let submit = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(submit.status(), reqwest::StatusCode::ACCEPTED);

    // The operator accepts — opening the on-chain vault — then changes its
    // mind and rejects the same job over its live session.
    let accept = http
        .post(format!("{base_url}/federation/jobs/{job_id}/accept"))
        .bearer_auth(&session)
        .json(&covenant_compute_protocol::JobAccept::Accept { job_id })
        .send()
        .await
        .unwrap();
    assert_eq!(accept.status(), reqwest::StatusCode::OK);
    assert_eq!(meter.opened().len(), 1, "the accept opened one vault");

    let reject = http
        .post(format!("{base_url}/federation/jobs/{job_id}/accept"))
        .bearer_auth(&session)
        .json(&covenant_compute_protocol::JobAccept::Reject {
            job_id,
            reason: "changed my mind after accepting".into(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(reject.status(), reqwest::StatusCode::OK);

    // The off-chain refund is matched on-chain: the vault opened at accept
    // is voided whole to the renter, never concluded for the seconds the
    // operator held it.
    assert_eq!(
        meter.voided(),
        vec![job_id],
        "rejecting an accepted lease voids the vault the accept opened"
    );
    assert!(
        meter.concluded().is_empty(),
        "a rejected lease is voided, never settled — nothing pays the operator"
    );
    assert_eq!(
        state_handle.escrow().status(job_id).await.unwrap(),
        EscrowStatus::Refunded
    );
    assert!(payout.records().is_empty(), "a rejected lease pays no one");
    assert_eq!(
        state_handle.jobs().get(job_id).unwrap().phase,
        covenant_compute_coordinator::JobPhase::Rejected
    );
}

const FEED_MINT: &str = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU";

/// A coordinator whose payout backend settles on a chain — a paid job
/// earns a citable signature — with the public proof feed toggled.
fn new_feed_state(feed_on: bool) -> (CoordinatorState, Arc<MockPayout>) {
    let identity = LocalIdentity::generate("coordinator@feed");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::with_onchain_mint(FEED_MINT));
    let config = CoordinatorConfig {
        long_poll_timeout: Duration::from_secs(5),
        default_funding_source: FundingSource::Organic,
        public_proof_feed: feed_on,
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::new(identity, config, reputation, payout.clone(), audit);
    (state, payout)
}

/// Settles `n` jobs through one operator — so every job matches to it and
/// `run_once` always has work — and returns their ids in submission order.
/// The multi-settlement fixture the batch feed commits over.
async fn settle_jobs(base_url: &str, coordinator_pubkey_b58: String, n: usize) -> Vec<Uuid> {
    let operator_identity = LocalIdentity::generate("operator@batch");
    let profile = cpu_profile(&operator_identity, 1_000);
    let coordinator_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.to_string(),
        Duration::from_secs(5),
        2,
    ));
    let register_req =
        RegisterRequest::sign(profile.clone(), payout_addr(7), &operator_identity).unwrap();
    assert!(
        coordinator_client
            .register(register_req)
            .await
            .unwrap()
            .accepted
    );
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
    let http = reqwest::Client::new();
    let mut ids = Vec::with_capacity(n);
    for _ in 0..n {
        let buyer = LocalIdentity::generate("buyer@batch");
        let job_id = Uuid::new_v4();
        let envelope = signed_envelope(&buyer, job_id, 1_000, 30_000, epoch_ms(), "batch-e2e");
        let resp = http
            .post(format!("{base_url}/federation/jobs"))
            .json(&envelope)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
        node.run_once().await.unwrap().unwrap();
        ids.push(job_id);
    }
    ids
}

/// Drives one job through the real match/execute/release/pay loop and
/// returns its id — the settled, on-chain-paid record the feed serves.
async fn settle_one_job(base_url: &str, coordinator_pubkey_b58: String) -> Uuid {
    let operator_identity = LocalIdentity::generate("operator@feed");
    let profile = cpu_profile(&operator_identity, 1_000);
    let coordinator_client = Arc::new(HttpCoordinatorClient::with_config(
        base_url.to_string(),
        Duration::from_secs(5),
        2,
    ));
    let register_req =
        RegisterRequest::sign(profile.clone(), payout_addr(7), &operator_identity).unwrap();
    assert!(
        coordinator_client
            .register(register_req)
            .await
            .unwrap()
            .accepted
    );
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
    let buyer = LocalIdentity::generate("buyer@feed");
    let job_id = Uuid::new_v4();
    let envelope = signed_envelope(&buyer, job_id, 1_000, 30_000, epoch_ms(), "feed-e2e");
    let http = reqwest::Client::new();
    let resp = http
        .post(format!("{base_url}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    node.run_once().await.unwrap().unwrap();
    job_id
}

#[tokio::test]
async fn the_proof_feed_serves_a_settled_job_and_it_verifies_offline() {
    let (state, _payout) = new_feed_state(true);
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let state_handle = state.clone();
    let base_url = spawn_coordinator(state).await;
    let job_id = settle_one_job(&base_url, coordinator_pubkey_b58).await;

    let record = state_handle.jobs().get(job_id).unwrap();
    assert_eq!(
        record.phase,
        covenant_compute_coordinator::JobPhase::Completed
    );
    assert!(
        record.payout.unwrap().tx_signature.is_some(),
        "the on-chain mock stamps a citable signature"
    );

    let http = reqwest::Client::new();
    // Anonymous read — no signed-read headers, unlike the buyer receipt poll.
    let feed_resp = http
        .get(format!("{base_url}/proof/receipts"))
        .send()
        .await
        .unwrap();
    assert_eq!(feed_resp.status(), reqwest::StatusCode::OK);
    let feed: Vec<serde_json::Value> = feed_resp.json().await.unwrap();
    assert_eq!(feed.len(), 1);
    let entry = &feed[0];
    assert_eq!(entry["job_id"], job_id.to_string());
    assert_eq!(entry["mint_b58"], FEED_MINT);
    assert!(entry.get("receipt").is_some());
    assert!(entry.get("tx_signature").is_some());
    assert!(
        entry.get("envelope").is_none(),
        "the buyer envelope must never be published"
    );
    assert!(
        entry.get("output").is_none(),
        "the job output must never be published"
    );

    // The published bundle verifies offline with no help from the coordinator.
    let proof: SettlementProof =
        serde_json::from_value(entry.clone()).expect("a well-formed proof");
    proof
        .verify_offline()
        .expect("a served proof verifies offline");
    assert_eq!(proof.payout_memo, proof.receipt.payout_memo());

    // The by-id route serves the same proof; an unknown job is 404.
    let one = http
        .get(format!("{base_url}/proof/receipts/{job_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(one.status(), reqwest::StatusCode::OK);
    let one_proof: SettlementProof = one.json().await.unwrap();
    assert_eq!(one_proof.job_id, job_id);
    let missing = http
        .get(format!("{base_url}/proof/receipts/{}", Uuid::new_v4()))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_proof_feed_is_404_until_a_deployment_opts_in() {
    let (state, _payout) = new_feed_state(false);
    let base_url = spawn_coordinator(state).await;
    let http = reqwest::Client::new();
    for path in [
        "/proof/receipts".to_string(),
        format!("/proof/receipts/{}", Uuid::new_v4()),
        "/proof/batch".to_string(),
        format!("/proof/batch/{}", Uuid::new_v4()),
    ] {
        assert_eq!(
            http.get(format!("{base_url}{path}"))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::NOT_FOUND,
            "{path} is 404 while the feed is off"
        );
    }
}

#[tokio::test]
async fn the_batch_feed_commits_settled_jobs_and_each_proves_inclusion() {
    let (state, _payout) = new_feed_state(true);
    let coordinator_pubkey_b58 = state.coordinator_pubkey_b58();
    let base_url = spawn_coordinator(state).await;

    let job_ids = settle_jobs(&base_url, coordinator_pubkey_b58, 3).await;

    let http = reqwest::Client::new();
    let batch: SettlementBatch = http
        .get(format!("{base_url}/proof/batch"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(batch.tree_size, 3);
    assert_eq!(batch.job_ids.len(), 3);
    assert_eq!(batch.root_hex.len(), 64);
    for id in &job_ids {
        assert!(batch.job_ids.contains(id), "batch lists every settled job");
    }

    // Every settled job proves inclusion under the pinned root, without
    // fetching the rest of the batch, and the settlement itself is sound.
    for id in &job_ids {
        let inclusion: BatchInclusionProof = http
            .get(format!("{base_url}/proof/batch/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(inclusion.proof.job_id, *id);
        inclusion
            .verify_offline(&batch.root_hex)
            .expect("inclusion verifies against the committed root");
        inclusion
            .proof
            .verify_offline()
            .expect("the cited settlement is self-verifying");
    }

    // A reader who pinned a different root rejects the same proof.
    let inclusion: BatchInclusionProof = http
        .get(format!("{base_url}/proof/batch/{}", job_ids[0]))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        inclusion.verify_offline(&"ab".repeat(32)).is_err(),
        "inclusion is bound to the real root"
    );

    // An unknown job has nothing to prove.
    let missing = http
        .get(format!("{base_url}/proof/batch/{}", Uuid::new_v4()))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
}

fn vault_coordinator() -> CoordinatorState {
    let identity = LocalIdentity::generate("coordinator@vault-e2e");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    let config = CoordinatorConfig {
        vault_enabled: true,
        ..CoordinatorConfig::default()
    };
    CoordinatorState::new(identity, config, reputation, payout, audit)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[tokio::test]
async fn a_vault_secret_round_trips_and_only_its_owner_can_reach_it() {
    let base = spawn_coordinator(vault_coordinator()).await;
    let http = reqwest::Client::new();

    let owner = LocalIdentity::generate("owner@vault-e2e");
    let owner_pk = owner.agent_id().pubkey_base58();
    let key = VaultKey::random();
    let secret_path = vault_secret_path(&owner_pk, "hf-token");
    let sealed = vault_seal(&key, b"hf_super_secret_token", secret_path.as_bytes()).unwrap();
    let body = serde_json::to_vec(&sealed).unwrap();
    let url = format!("{base}{secret_path}");

    // Store: the signature binds the method, the path, and the exact body.
    let ts = now_ms();
    let sig = sign_vault(&owner, &vault_signing_path("POST", &secret_path), &body, ts).unwrap();
    let put = http
        .post(&url)
        .header(VAULT_SIGNED_AT_HEADER, ts.to_string())
        .header(VAULT_SIGNATURE_HEADER, &sig)
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), reqwest::StatusCode::NO_CONTENT);

    // Fetch: the coordinator returns the same opaque ciphertext, and only
    // the owner's key opens it back to the plaintext.
    let ts = now_ms();
    let sig = sign_vault(&owner, &vault_signing_path("GET", &secret_path), &[], ts).unwrap();
    let got = http
        .get(&url)
        .header(VAULT_SIGNED_AT_HEADER, ts.to_string())
        .header(VAULT_SIGNATURE_HEADER, &sig)
        .send()
        .await
        .unwrap();
    assert_eq!(got.status(), reqwest::StatusCode::OK);
    let fetched: SealedSecret = got.json().await.unwrap();
    assert_eq!(fetched, sealed);
    assert_eq!(
        vault_open(&key, &fetched, secret_path.as_bytes()).unwrap(),
        b"hf_super_secret_token"
    );

    // A listing names the label but never the ciphertext.
    let list_path = vault_list_path(&owner_pk);
    let ts = now_ms();
    let sig = sign_vault(&owner, &vault_signing_path("GET", &list_path), &[], ts).unwrap();
    let listed: serde_json::Value = http
        .get(format!("{base}{list_path}"))
        .header(VAULT_SIGNED_AT_HEADER, ts.to_string())
        .header(VAULT_SIGNATURE_HEADER, &sig)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed[0]["label"], "hf-token");
    assert!(listed[0].get("ciphertext").is_none() && listed[0].get("nonce").is_none());

    // An intruder who signs the owner's path with their own key is refused
    // — possession of the owner's key is exactly what the path demands.
    let intruder = LocalIdentity::generate("intruder@vault-e2e");
    let ts = now_ms();
    let isig = sign_vault(&intruder, &vault_signing_path("GET", &secret_path), &[], ts).unwrap();
    let refused = http
        .get(&url)
        .header(VAULT_SIGNED_AT_HEADER, ts.to_string())
        .header(VAULT_SIGNATURE_HEADER, &isig)
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::UNAUTHORIZED);

    // A store whose signed body differs from the body sent is refused, so
    // a captured request cannot be replayed with a substituted secret.
    let ts = now_ms();
    let sig = sign_vault(&owner, &vault_signing_path("POST", &secret_path), &body, ts).unwrap();
    let other_body =
        serde_json::to_vec(&vault_seal(&key, b"different", secret_path.as_bytes()).unwrap())
            .unwrap();
    let tampered = http
        .post(&url)
        .header(VAULT_SIGNED_AT_HEADER, ts.to_string())
        .header(VAULT_SIGNATURE_HEADER, &sig)
        .body(other_body)
        .send()
        .await
        .unwrap();
    assert_eq!(tampered.status(), reqwest::StatusCode::UNAUTHORIZED);

    // Delete: the secret is gone, and a fetch afterwards is a 404.
    let ts = now_ms();
    let sig = sign_vault(&owner, &vault_signing_path("DELETE", &secret_path), &[], ts).unwrap();
    let deleted = http
        .delete(&url)
        .header(VAULT_SIGNED_AT_HEADER, ts.to_string())
        .header(VAULT_SIGNATURE_HEADER, &sig)
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), reqwest::StatusCode::NO_CONTENT);

    let ts = now_ms();
    let sig = sign_vault(&owner, &vault_signing_path("GET", &secret_path), &[], ts).unwrap();
    let after = http
        .get(&url)
        .header(VAULT_SIGNED_AT_HEADER, ts.to_string())
        .header(VAULT_SIGNATURE_HEADER, &sig)
        .send()
        .await
        .unwrap();
    assert_eq!(after.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_vault_read_signature_cannot_be_replayed_as_a_delete() {
    let base = spawn_coordinator(vault_coordinator()).await;
    let http = reqwest::Client::new();

    let owner = LocalIdentity::generate("owner@vault-verb");
    let owner_pk = owner.agent_id().pubkey_base58();
    let key = VaultKey::random();
    let secret_path = vault_secret_path(&owner_pk, "deploy-key");
    let sealed = vault_seal(&key, b"a-real-secret", secret_path.as_bytes()).unwrap();
    let body = serde_json::to_vec(&sealed).unwrap();
    let url = format!("{base}{secret_path}");

    let ts = now_ms();
    let sig = sign_vault(&owner, &vault_signing_path("POST", &secret_path), &body, ts).unwrap();
    let put = http
        .post(&url)
        .header(VAULT_SIGNED_AT_HEADER, ts.to_string())
        .header(VAULT_SIGNATURE_HEADER, &sig)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), reqwest::StatusCode::NO_CONTENT);

    // The owner signs a fetch of the secret, then that exact, still-fresh
    // signature is presented on a DELETE of the same path. The method is
    // part of what the owner signed, so the coordinator refuses it: a
    // captured read is not a licence to destroy.
    let ts = now_ms();
    let read_sig = sign_vault(&owner, &vault_signing_path("GET", &secret_path), &[], ts).unwrap();
    let replayed = http
        .delete(&url)
        .header(VAULT_SIGNED_AT_HEADER, ts.to_string())
        .header(VAULT_SIGNATURE_HEADER, &read_sig)
        .send()
        .await
        .unwrap();
    assert_eq!(replayed.status(), reqwest::StatusCode::UNAUTHORIZED);

    // The secret the forged delete aimed at is untouched.
    let ts = now_ms();
    let read_sig = sign_vault(&owner, &vault_signing_path("GET", &secret_path), &[], ts).unwrap();
    let still_there = http
        .get(&url)
        .header(VAULT_SIGNED_AT_HEADER, ts.to_string())
        .header(VAULT_SIGNATURE_HEADER, &read_sig)
        .send()
        .await
        .unwrap();
    assert_eq!(still_there.status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn a_disabled_vault_serves_no_routes() {
    let (state, _payout) = new_coordinator_state(Duration::from_secs(2));
    let base = spawn_coordinator(state).await;
    let http = reqwest::Client::new();
    let owner = LocalIdentity::generate("owner@vault-off");
    let owner_pk = owner.agent_id().pubkey_base58();
    let secret_path = vault_secret_path(&owner_pk, "k");
    // Even a correctly signed request 404s when the deployment did not opt
    // in — the vault is simply not a surface this coordinator serves.
    let ts = now_ms();
    let sig = sign_vault(&owner, &vault_signing_path("GET", &secret_path), &[], ts).unwrap();
    let resp = http
        .get(format!("{base}{secret_path}"))
        .header(VAULT_SIGNED_AT_HEADER, ts.to_string())
        .header(VAULT_SIGNATURE_HEADER, &sig)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_buyer_vault_helpers_seal_store_fetch_and_delete() {
    let base = spawn_coordinator(vault_coordinator()).await;
    let http = reqwest::Client::new();
    let config = covenant_compute_buyer::BuyerConfig {
        coordinator_url: base,
        poll_interval: Duration::from_millis(100),
        referral_code: None,
        rpc_url: None,
    };
    let buyer = LocalIdentity::generate("buyer@vault-helpers");
    let key = VaultKey::random();

    covenant_compute_buyer::vault_store(&http, &config, &buyer, &key, "openai-key", b"sk-live-xyz")
        .await
        .unwrap();

    let listed = covenant_compute_buyer::vault_list(&http, &config, &buyer)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].label, "openai-key");

    let opened = covenant_compute_buyer::vault_fetch(&http, &config, &buyer, &key, "openai-key")
        .await
        .unwrap();
    assert_eq!(opened, b"sk-live-xyz");

    // The key is what opens the secret; the network holding the ciphertext
    // does not, and neither does the wrong key.
    let wrong = VaultKey::random();
    assert!(
        covenant_compute_buyer::vault_fetch(&http, &config, &buyer, &wrong, "openai-key")
            .await
            .is_err()
    );

    covenant_compute_buyer::vault_delete(&http, &config, &buyer, "openai-key")
        .await
        .unwrap();
    assert!(
        covenant_compute_buyer::vault_fetch(&http, &config, &buyer, &key, "openai-key")
            .await
            .is_err(),
        "the secret is gone after delete"
    );
}

async fn signed_vault_store(
    http: &reqwest::Client,
    base: &str,
    owner: &LocalIdentity,
    label: &str,
) -> reqwest::StatusCode {
    let owner_pk = owner.agent_id().pubkey_base58();
    let path = vault_secret_path(&owner_pk, label);
    let body = serde_json::to_vec(&vault_seal(&VaultKey::random(), b"v", path.as_bytes()).unwrap())
        .unwrap();
    let ts = now_ms();
    let sig = sign_vault(owner, &vault_signing_path("POST", &path), &body, ts).unwrap();
    http.post(format!("{base}{path}"))
        .header(VAULT_SIGNED_AT_HEADER, ts.to_string())
        .header(VAULT_SIGNATURE_HEADER, &sig)
        .body(body)
        .send()
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn a_full_vault_turns_a_new_owner_away_but_not_a_known_one() {
    let identity = LocalIdentity::generate("coordinator@vault-cap");
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    let config = CoordinatorConfig {
        vault_enabled: true,
        vault_max_owners: Some(1),
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::new(identity, config, reputation, payout, audit);
    let base = spawn_coordinator(state).await;
    let http = reqwest::Client::new();

    let first = LocalIdentity::generate("first@vault-cap");
    assert_eq!(
        signed_vault_store(&http, &base, &first, "k").await,
        reqwest::StatusCode::NO_CONTENT
    );
    // A second distinct owner is over the ceiling.
    let second = LocalIdentity::generate("second@vault-cap");
    assert_eq!(
        signed_vault_store(&http, &base, &second, "k").await,
        reqwest::StatusCode::TOO_MANY_REQUESTS
    );
    // The admitted owner keeps writing.
    assert_eq!(
        signed_vault_store(&http, &base, &first, "k2").await,
        reqwest::StatusCode::NO_CONTENT
    );
}
