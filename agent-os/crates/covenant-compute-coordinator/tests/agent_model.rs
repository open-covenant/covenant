//! Agent work over the real HTTP surface: an agent task's result is held
//! until a check ordered from another operator returns, and the task pays,
//! refunds, or waits on exactly what that check says.
//!
//! The coordinator runs in-process on a loopback port. Operators are
//! registered straight into the registry and answer through the result
//! endpoint with signed receipts, so every money decision here is the one
//! production makes.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency, A2ATaskStatus};
use covenant_audit::{AuditKind, AuditLog, InMemoryAuditLog};
use covenant_compute_coordinator::{
    router, AgentPolicy, AuditReputationSource, CoordinatorConfig, CoordinatorState, JobPhase,
    MockPayout, NoopVoteRounds, RoundResult, SubsidyPolicy, VoteRounds,
};
use covenant_compute_protocol::{
    agent_check_output, agent_task_input, agent_task_output, parse_agent_check, sha256_hex,
    AcceptanceSpec, AgentCheckVerdict, AgentRuntime, AgentSkill, AgentTaskOutput, AgentTaskSpec,
    CapabilityProfile, CapabilityRequirement, CommandOutcome, EscrowStatus, FederationEscrow,
    FundingSource, HardwareClass, JobEnvelopePayload, JobKind, JobMeter, JobResultAck,
    JobResultMessage, KindAsk, KindModels, PriceAsk, PriceUnit, RegisterRequest, RepoSource,
    ResultSettlement, SignedJobEnvelope, SignedWorkReceipt, WorkReceiptPayload,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use uuid::Uuid;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const COMMAND: &str = "python -m unittest discover -s tests -t .";
const TASK_PRICE: u64 = 100_000;

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

struct Rig {
    state: CoordinatorState,
    payout: Arc<MockPayout>,
    url: String,
    http: reqwest::Client,
}

async fn rig() -> Rig {
    rig_with(120_000).await
}

async fn rig_with(accept_timeout_ms: u64) -> Rig {
    rig_sampling(accept_timeout_ms, 0).await
}

async fn rig_sampling(accept_timeout_ms: u64, pass_sample_bps: u32) -> Rig {
    rig_full(accept_timeout_ms, pass_sample_bps, None).await
}

async fn rig_rounds(rounds: Arc<NoopVoteRounds>) -> Rig {
    rig_full(120_000, 0, Some(rounds)).await
}

async fn rig_full(
    accept_timeout_ms: u64,
    pass_sample_bps: u32,
    rounds: Option<Arc<NoopVoteRounds>>,
) -> Rig {
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
    let payout = Arc::new(MockPayout::new());
    let config = CoordinatorConfig {
        long_poll_timeout: Duration::from_secs(1),
        default_funding_source: FundingSource::Organic,
        subsidy_policy: Some(SubsidyPolicy::new(10_000, 1_000_000).unwrap()),
        agent: Some(AgentPolicy {
            buyers: vec!["*".into()],
            check_price_micro_usdc: 1_000,
            max_check_attempts: 5,
            check_accept_timeout_ms: accept_timeout_ms,
            confirm_failures: true,
            pass_sample_bps,
            build_markup_bps: 12_000,
            build_floor_micro_usdc: 10_000,
        }),
        vote_rounds: rounds.map(|r| r as Arc<dyn VoteRounds>),
        ..CoordinatorConfig::default()
    };
    let state = CoordinatorState::new(
        LocalIdentity::generate("coordinator@agent"),
        config,
        reputation,
        payout.clone(),
        audit,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let served = state.clone();
    tokio::spawn(async move { axum::serve(listener, router(served)).await.unwrap() });
    Rig {
        state,
        payout,
        url,
        http: reqwest::Client::new(),
    }
}

fn register(rig: &Rig, display: &str, seed: u8) -> LocalIdentity {
    let op = LocalIdentity::generate(display);
    let profile = CapabilityProfile {
        operator: op.agent_id(),
        hardware: HardwareClass::CpuOnly,
        vram_gb: 0,
        models_served: vec!["claude-code".into(), "python:3.12-slim".into()],
        job_kinds: vec![JobKind::AgentTask, JobKind::AgentCheck],
        // Builds are asked well above the check price, so a check only
        // matches through the seat's own per-kind ask.
        price: PriceAsk {
            unit: PriceUnit::PerJob,
            micro_usdc: 20_000,
        },
        tee_capable: false,
        kind_prices: vec![KindAsk {
            kind: JobKind::AgentCheck,
            price: PriceAsk {
                unit: PriceUnit::PerJob,
                micro_usdc: 1_000,
            },
        }],
        kind_models: vec![
            KindModels {
                kind: JobKind::AgentTask,
                models: vec!["claude-code".into()],
            },
            KindModels {
                kind: JobKind::AgentCheck,
                models: vec!["python:3.12-slim".into()],
            },
        ],
    };
    let payout = bs58::encode([seed; 32]).into_string();
    rig.state
        .registry()
        .register(
            &RegisterRequest::sign(profile, payout, &op).unwrap(),
            epoch_ms(),
            None,
            false,
        )
        .unwrap();
    op
}

fn spec() -> AgentTaskSpec {
    AgentTaskSpec {
        task: "implement slugify".into(),
        repo: RepoSource::Bundle {
            bundle_b64: "QUJD".into(),
            commit: COMMIT.into(),
        },
        acceptance: AcceptanceSpec {
            skill: AgentSkill::CodeChange,
            image: "python:3.12-slim".into(),
            commands: vec![COMMAND.into()],
            timeout_secs: 120,
            protected_paths: vec!["tests/".into()],
            hidden_sha256: None,
        },
        runtime: AgentRuntime::ClaudeCode,
        model: None,
    }
}

async fn post_task(rig: &Rig) -> Uuid {
    post_task_with(rig, spec()).await
}

async fn post_task_with(rig: &Rig, spec: AgentTaskSpec) -> Uuid {
    let buyer = LocalIdentity::generate("buyer@agent");
    let job_id = Uuid::new_v4();
    let payload = JobEnvelopePayload {
        job_id,
        buyer: buyer.agent_id(),
        kind: JobKind::AgentTask,
        capability_requirement: CapabilityRequirement {
            gpu_class: None,
            min_vram_gb: None,
            model_id: Some("claude-code".into()),
            kind: JobKind::AgentTask,
            max_duration_secs: 600,
            min_reputation_bps: None,
        },
        input: vec![agent_task_input(spec).unwrap()],
        price_micro_usdc: TASK_PRICE,
        deadline_ms: 1_200_000,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, job_id.to_string()),
        issued_at_ms: epoch_ms(),
        referral_code: None,
        stream: false,
    };
    let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
    let resp = rig
        .http
        .post(format!("{}/federation/jobs", rig.url))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::ACCEPTED,
        "{}",
        resp.text().await.unwrap()
    );
    job_id
}

async fn submit(
    rig: &Rig,
    job_id: Uuid,
    operator: &LocalIdentity,
    output: Vec<Content>,
) -> JobResultAck {
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
    let resp = rig
        .http
        .post(format!("{}/federation/jobs/{job_id}/result", rig.url))
        .json(&JobResultMessage { receipt, output })
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );
    resp.json().await.unwrap()
}

fn patch_spending(spend_micro_usd: u64) -> AgentTaskOutput {
    AgentTaskOutput {
        spend_micro_usd,
        ..patch()
    }
}

fn patch() -> AgentTaskOutput {
    let bytes = b"diff --git a/slugify.py b/slugify.py\n";
    AgentTaskOutput {
        base_commit: COMMIT.into(),
        patch_b64: base64::engine::general_purpose::STANDARD.encode(bytes),
        patch_sha256: sha256_hex(bytes),
        files_changed: 1,
        summary: "implemented slugify".into(),
        model: None,
        spend_micro_usd: 0,
        guard_run_id: None,
    }
}

fn verdict(task_id: Uuid, sha: &str, exit_code: i32) -> Vec<Content> {
    verdict_over(task_id, sha, &[COMMAND], exit_code)
}

fn verdict_over(task_id: Uuid, sha: &str, commands: &[&str], exit_code: i32) -> Vec<Content> {
    agent_check_output(AgentCheckVerdict::new(
        task_id,
        sha.into(),
        true,
        Vec::new(),
        commands
            .iter()
            .map(|command| CommandOutcome {
                command: (*command).into(),
                exit_code,
                duration_ms: 200,
                timed_out: false,
                output_tail: if exit_code == 0 {
                    "OK".into()
                } else {
                    "FAILED".into()
                },
            })
            .collect(),
    ))
    .unwrap()
}

/// A passing or failing verdict whose vote `checker` signed.
fn signed_verdict(task_id: Uuid, checker: &LocalIdentity, pass: bool) -> Vec<Content> {
    let exit_code = if pass { 0 } else { 1 };
    let verdict = AgentCheckVerdict::new(
        task_id,
        patch().patch_sha256,
        true,
        Vec::new(),
        vec![CommandOutcome {
            command: COMMAND.into(),
            exit_code,
            duration_ms: 200,
            timed_out: false,
            output_tail: String::new(),
        }],
    );
    agent_check_output(verdict.sign_vote(checker)).unwrap()
}

/// The check job ordered for `task_id`, with the operator it went to.
fn latest_check(rig: &Rig, task_id: Uuid) -> (Uuid, String) {
    let task = rig.state.jobs().get(task_id).unwrap();
    let check_id = *task.check_jobs.last().expect("a check was ordered");
    let check = rig.state.jobs().get(check_id).unwrap();
    assert_eq!(check.checks_task, Some(task_id));
    assert!(
        check.pinned,
        "a check goes to the operator it was ordered from"
    );
    (check_id, check.operator_pubkey_b58)
}

async fn settled(rig: &Rig, task_id: Uuid) -> JobPhase {
    for _ in 0..100 {
        let phase = rig.state.jobs().get(task_id).unwrap().phase;
        if phase != JobPhase::AwaitingCheck {
            return phase;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    JobPhase::AwaitingCheck
}

#[tokio::test]
async fn an_agent_task_pays_only_after_another_operators_check_passes() {
    let rig = rig().await;
    let builder = register(&rig, "builder@agent", 1);
    let task_id = post_task(&rig).await;
    let checker = register(&rig, "checker@agent", 2);

    let ack = submit(&rig, task_id, &builder, agent_task_output(patch()).unwrap()).await;
    assert_eq!(ack.settled, ResultSettlement::AwaitingCheck);
    assert_eq!(ack.released_gross_micro_usdc, 0);
    assert_eq!(
        rig.state.escrow().status(task_id).await.unwrap(),
        EscrowStatus::Held,
        "a verified result alone moves no money"
    );
    assert!(rig.payout.records().is_empty());

    let (check_id, assignee) = latest_check(&rig, task_id);
    assert_eq!(
        assignee,
        checker.agent_id().pubkey_base58(),
        "never the builder"
    );
    let check = rig.state.jobs().get(check_id).unwrap();
    let ordered = parse_agent_check(&check.envelope.payload.input).unwrap();
    assert_eq!(ordered.patch_sha256, patch().patch_sha256);
    assert_eq!(check.escrow_hold.funding_source, FundingSource::Bootstrap);

    submit(
        &rig,
        check_id,
        &checker,
        verdict(task_id, &patch().patch_sha256, 0),
    )
    .await;
    assert_eq!(settled(&rig, task_id).await, JobPhase::Completed);
    assert_eq!(
        rig.state.escrow().status(task_id).await.unwrap(),
        EscrowStatus::Released
    );
    let builder_key = builder.agent_id().pubkey_base58();
    let paid: Vec<_> = rig
        .payout
        .records()
        .into_iter()
        .filter(|r| r.job_id == task_id)
        .collect();
    assert_eq!(paid.len(), 1, "the builder is paid exactly once");
    assert_eq!(paid[0].operator_pubkey_b58, builder_key);
    // The build reported no spend, so it is paid the floor; the buyer pays
    // that and the one check, and the rest of the offer stays theirs.
    assert_eq!(paid[0].amount_micro_usdc, 10_000);
    assert_eq!(
        rig.state.escrow().settled_charge_micro_usdc(task_id),
        Some(11_000)
    );

    // A late redelivery of the builder's result echoes the settlement and
    // orders nothing new.
    let replay = submit(&rig, task_id, &builder, agent_task_output(patch()).unwrap()).await;
    assert_eq!(replay.settled, ResultSettlement::Released);
    assert_eq!(rig.state.jobs().get(task_id).unwrap().check_jobs.len(), 1);
}

#[tokio::test]
async fn a_failed_check_refunds_the_buyer_and_faults_the_builder() {
    let rig = rig().await;
    let builder = register(&rig, "builder@agent", 1);
    let task_id = post_task(&rig).await;
    let checker = register(&rig, "checker@agent", 2);

    submit(&rig, task_id, &builder, agent_task_output(patch()).unwrap()).await;
    let (check_id, _) = latest_check(&rig, task_id);
    submit(
        &rig,
        check_id,
        &checker,
        verdict(task_id, &patch().patch_sha256, 1),
    )
    .await;

    assert_eq!(settled(&rig, task_id).await, JobPhase::Refunded);
    let task = rig.state.jobs().get(task_id).unwrap();
    assert_eq!(
        task.refund_reason,
        Some(covenant_compute_protocol::RefundReason::CheckFailed)
    );
    assert_eq!(
        rig.state.escrow().status(task_id).await.unwrap(),
        EscrowStatus::Refunded
    );
    assert!(
        rig.payout.records().iter().all(|r| r.job_id != task_id),
        "work that failed its check earns nothing"
    );
    let builder_key = builder.agent_id().pubkey_base58();
    let events = rig.state.audit().recent(usize::MAX).await.unwrap();
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            AuditKind::ComputeJobRefunded { job_id, reason, operator_pubkey_b58: Some(op) }
                if *job_id == task_id && reason == "check_failed" && *op == builder_key
        )),
        "the refund is the builder's fault on the audit chain"
    );
}

#[tokio::test]
async fn a_verdict_on_another_patch_never_pays() {
    let rig = rig().await;
    let builder = register(&rig, "builder@agent", 1);
    let task_id = post_task(&rig).await;
    let checker = register(&rig, "checker@agent", 2);

    submit(&rig, task_id, &builder, agent_task_output(patch()).unwrap()).await;
    let (check_id, _) = latest_check(&rig, task_id);
    let other = "bb".repeat(32);
    submit(&rig, check_id, &checker, verdict(task_id, &other, 0)).await;

    // The passing verdict names other bytes, so it is no verdict at all:
    // the task stays held, and with no third operator to order another
    // check from, it stays held rather than paying.
    tokio::time::sleep(Duration::from_millis(300)).await;
    covenant_compute_coordinator::agent::settle(&rig.state, task_id).await;
    assert_eq!(
        rig.state.jobs().get(task_id).unwrap().phase,
        JobPhase::AwaitingCheck
    );
    assert_eq!(
        rig.state.escrow().status(task_id).await.unwrap(),
        EscrowStatus::Held
    );
    assert!(rig.payout.records().iter().all(|r| r.job_id != task_id));
}

#[tokio::test]
async fn a_malformed_result_is_the_builders_failure_and_orders_no_check() {
    let rig = rig().await;
    let builder = register(&rig, "builder@agent", 1);
    let task_id = post_task(&rig).await;
    register(&rig, "checker@agent", 2);

    let mut lying = patch();
    lying.patch_sha256 = "cc".repeat(32);
    let output = vec![Content::json(
        serde_json::json!({ "agent_task_result": lying }),
    )];
    let ack = submit(&rig, task_id, &builder, output).await;
    assert_eq!(ack.settled, ResultSettlement::Refunded);
    let task = rig.state.jobs().get(task_id).unwrap();
    assert_eq!(task.phase, JobPhase::Failed);
    assert!(
        task.check_jobs.is_empty(),
        "no check is spent on a patch that is not one"
    );
    assert_eq!(
        rig.state.escrow().status(task_id).await.unwrap(),
        EscrowStatus::Refunded
    );
}

#[tokio::test]
async fn agent_work_is_closed_to_unlisted_buyers_and_checks_cannot_be_bought() {
    let rig = rig().await;
    let closed = {
        let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
        let reputation = Arc::new(AuditReputationSource::new(audit.clone()));
        let config = CoordinatorConfig {
            agent: Some(AgentPolicy {
                buyers: vec!["someone-else".into()],
                check_price_micro_usdc: 1_000,
                max_check_attempts: 5,
                check_accept_timeout_ms: 120_000,
                confirm_failures: true,
                pass_sample_bps: 0,
                build_markup_bps: 12_000,
                build_floor_micro_usdc: 10_000,
            }),
            ..CoordinatorConfig::default()
        };
        let state = CoordinatorState::new(
            LocalIdentity::generate("coordinator@closed"),
            config,
            reputation,
            Arc::new(MockPayout::new()),
            audit,
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
        url
    };
    let buyer = LocalIdentity::generate("buyer@agent");
    let job_id = Uuid::new_v4();
    let payload = JobEnvelopePayload {
        job_id,
        buyer: buyer.agent_id(),
        kind: JobKind::AgentTask,
        capability_requirement: CapabilityRequirement {
            gpu_class: None,
            min_vram_gb: None,
            model_id: Some("claude-code".into()),
            kind: JobKind::AgentTask,
            max_duration_secs: 600,
            min_reputation_bps: None,
        },
        input: vec![agent_task_input(spec()).unwrap()],
        price_micro_usdc: TASK_PRICE,
        deadline_ms: 1_200_000,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, job_id.to_string()),
        issued_at_ms: epoch_ms(),
        referral_code: None,
        stream: false,
    };
    let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
    let resp = rig
        .http
        .post(format!("{closed}/federation/jobs"))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    let check =
        covenant_compute_protocol::agent_check_input(covenant_compute_protocol::AgentCheckSpec {
            task_job_id: Uuid::new_v4(),
            repo: spec().repo,
            acceptance: spec().acceptance,
            patch_b64: patch().patch_b64,
            patch_sha256: patch().patch_sha256,
            hidden: None,
        })
        .unwrap();
    let check_id = Uuid::new_v4();
    let payload = JobEnvelopePayload {
        job_id: check_id,
        buyer: buyer.agent_id(),
        kind: JobKind::AgentCheck,
        capability_requirement: CapabilityRequirement {
            gpu_class: None,
            min_vram_gb: None,
            model_id: Some("python:3.12-slim".into()),
            kind: JobKind::AgentCheck,
            max_duration_secs: 600,
            min_reputation_bps: None,
        },
        input: vec![check],
        price_micro_usdc: 1_000,
        deadline_ms: 600_000,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, check_id.to_string()),
        issued_at_ms: epoch_ms(),
        referral_code: None,
        stream: false,
    };
    let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
    let resp = rig
        .http
        .post(format!("{}/federation/jobs", rig.url))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn capacity_lists_each_agent_kind_with_its_own_models_and_ask() {
    let rig = rig().await;
    register(&rig, "seat@agent", 1);
    let view: covenant_compute_protocol::CapacityView = rig
        .http
        .get(format!("{}/federation/capacity", rig.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rows: Vec<(JobKind, String, u64)> = view
        .entries
        .iter()
        .map(|e| (e.kind, e.model.clone(), e.min_ask_micro_usdc))
        .collect();
    assert_eq!(
        rows.len(),
        2,
        "one row per kind and its own model, nothing crossed: {rows:?}"
    );
    assert!(rows.contains(&(JobKind::AgentTask, "claude-code".into(), 20_000)));
    assert!(rows.contains(&(JobKind::AgentCheck, "python:3.12-slim".into(), 1_000)));
}

#[tokio::test]
async fn a_check_nobody_picks_up_moves_to_another_operator() {
    let rig = rig_with(0).await;
    let builder = register(&rig, "builder@agent", 1);
    let task_id = post_task(&rig).await;
    register(&rig, "checker-a@agent", 2);
    register(&rig, "checker-b@agent", 3);

    submit(&rig, task_id, &builder, agent_task_output(patch()).unwrap()).await;
    let (first_id, first_checker) = latest_check(&rig, task_id);
    tokio::time::sleep(Duration::from_millis(5)).await;
    covenant_compute_coordinator::agent::settle(&rig.state, task_id).await;

    let first = rig.state.jobs().get(first_id).unwrap();
    assert_eq!(
        first.phase,
        JobPhase::Refunded,
        "the unclaimed check is withdrawn"
    );
    assert_eq!(
        rig.state.escrow().status(first_id).await.unwrap(),
        EscrowStatus::Refunded
    );
    let (second_id, second_checker) = latest_check(&rig, task_id);
    assert_ne!(second_id, first_id);
    assert_ne!(
        second_checker, first_checker,
        "never the checker that sat on it"
    );
    assert_ne!(second_checker, builder.agent_id().pubkey_base58());
    assert_eq!(
        rig.state.jobs().get(task_id).unwrap().phase,
        JobPhase::AwaitingCheck
    );
}

#[tokio::test]
async fn hidden_checks_reach_only_the_checker_and_gate_the_payment() {
    let hidden = covenant_compute_protocol::HiddenChecks {
        files: vec![covenant_compute_protocol::HiddenFile {
            path: "tests/hidden/test_edges.py".into(),
            content_b64: base64::engine::general_purpose::STANDARD.encode("import unittest\n"),
        }],
        commands: vec!["python -m unittest tests.hidden.test_edges".into()],
    };
    let mut committed = spec();
    committed.acceptance.hidden_sha256 = Some(hidden.digest());

    let rig = rig().await;
    let builder = register(&rig, "builder@agent", 1);
    let task_id = post_task_with(&rig, committed).await;
    let checker = register(&rig, "checker@agent", 2);

    let offered = rig.state.jobs().get(task_id).unwrap();
    let offered_json = serde_json::to_string(&offered.envelope).unwrap();
    assert!(
        !offered_json.contains("test_edges"),
        "the builder's offer carries the commitment, never the checks"
    );

    submit(&rig, task_id, &builder, agent_task_output(patch()).unwrap()).await;
    assert!(
        rig.state.jobs().get(task_id).unwrap().check_jobs.is_empty(),
        "no check is ordered before the buyer hands its hidden checks over"
    );

    let url = format!("{}/federation/jobs/{task_id}/hidden", rig.url);
    let mut swapped = hidden.clone();
    swapped.commands = vec!["true".into()];
    let refused = rig.http.post(&url).json(&swapped).send().await.unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::BAD_REQUEST);
    let taken = rig.http.post(&url).json(&hidden).send().await.unwrap();
    assert_eq!(taken.status(), reqwest::StatusCode::OK);

    let mut ordered = None;
    for _ in 0..100 {
        if let Some(id) = rig.state.jobs().get(task_id).unwrap().check_jobs.last() {
            ordered = Some(*id);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let check_id = ordered.expect("handing the checks over orders the check");
    let check = rig.state.jobs().get(check_id).unwrap();
    let ordered_spec = parse_agent_check(&check.envelope.payload.input).unwrap();
    assert_eq!(ordered_spec.hidden, Some(hidden.clone()));

    // A pass that skipped the hidden command is no verdict.
    submit(
        &rig,
        check_id,
        &checker,
        verdict_over(task_id, &patch().patch_sha256, &[COMMAND], 0),
    )
    .await;
    covenant_compute_coordinator::agent::settle(&rig.state, task_id).await;
    assert!(rig.payout.records().iter().all(|r| r.job_id != task_id));
    assert_eq!(
        rig.state.jobs().get(task_id).unwrap().phase,
        JobPhase::AwaitingCheck
    );
}

/// Answers the task's latest check as whichever registered checker it went
/// to, passing or failing the patch.
async fn answer_latest(
    rig: &Rig,
    task_id: Uuid,
    checkers: &[&LocalIdentity],
    pass: bool,
) -> String {
    let (check_id, assignee) = latest_check(rig, task_id);
    let checker = checkers
        .iter()
        .find(|c| c.agent_id().pubkey_base58() == assignee)
        .expect("the check went to a registered checker");
    let exit_code = if pass { 0 } else { 1 };
    submit(
        rig,
        check_id,
        checker,
        verdict(task_id, &patch().patch_sha256, exit_code),
    )
    .await;
    covenant_compute_coordinator::agent::settle(&rig.state, task_id).await;
    assignee
}

fn agreement(
    rig_events: &[covenant_audit::AuditEvent],
    task_id: Uuid,
    checker: &str,
) -> Option<bool> {
    rig_events.iter().find_map(|e| match &e.kind {
        AuditKind::ComputeRedundancyResult {
            source_job_id,
            operator_pubkey_b58,
            agreed,
            ..
        } if *source_job_id == task_id && operator_pubkey_b58 == checker => *agreed,
        _ => None,
    })
}

#[tokio::test]
async fn a_failed_check_is_confirmed_and_a_lying_checker_is_outvoted() {
    let rig = rig().await;
    let builder = register(&rig, "builder@agent", 1);
    let task_id = post_task(&rig).await;
    let a = register(&rig, "checker-a@agent", 2);
    let b = register(&rig, "checker-b@agent", 3);

    submit(&rig, task_id, &builder, agent_task_output(patch()).unwrap()).await;
    let liar = answer_latest(&rig, task_id, &[&a, &b], false).await;
    assert_eq!(
        rig.state.jobs().get(task_id).unwrap().phase,
        JobPhase::AwaitingCheck,
        "one failing verdict is confirmed before anyone is refused"
    );
    let honest = answer_latest(&rig, task_id, &[&a, &b], true).await;
    assert_ne!(liar, honest);
    assert_eq!(
        rig.state.jobs().get(task_id).unwrap().phase,
        JobPhase::AwaitingCheck,
        "a split asks for a third check and waits for one"
    );

    let c = register(&rig, "checker-c@agent", 4);
    covenant_compute_coordinator::agent::settle(&rig.state, task_id).await;
    let third = answer_latest(&rig, task_id, &[&a, &b, &c], true).await;
    assert_eq!(third, c.agent_id().pubkey_base58());
    assert_eq!(settled(&rig, task_id).await, JobPhase::Completed);
    assert!(rig.payout.records().iter().any(|r| r.job_id == task_id));

    let events = rig.state.audit().recent(usize::MAX).await.unwrap();
    assert_eq!(
        agreement(&events, task_id, &liar),
        Some(false),
        "the outvoted checker is faulted"
    );
    assert_eq!(agreement(&events, task_id, &honest), Some(true));
}

#[tokio::test]
async fn two_failing_checks_refuse_the_builder() {
    let rig = rig().await;
    let builder = register(&rig, "builder@agent", 1);
    let task_id = post_task(&rig).await;
    let a = register(&rig, "checker-a@agent", 2);
    let b = register(&rig, "checker-b@agent", 3);

    submit(&rig, task_id, &builder, agent_task_output(patch()).unwrap()).await;
    answer_latest(&rig, task_id, &[&a, &b], false).await;
    answer_latest(&rig, task_id, &[&a, &b], false).await;
    assert_eq!(settled(&rig, task_id).await, JobPhase::Refunded);
    assert_eq!(
        rig.state.jobs().get(task_id).unwrap().refund_reason,
        Some(covenant_compute_protocol::RefundReason::CheckFailed)
    );
    assert!(rig.payout.records().iter().all(|r| r.job_id != task_id));
}

#[tokio::test]
async fn a_sampled_pass_is_confirmed_before_payment() {
    let rig = rig_sampling(120_000, 10_000).await;
    let builder = register(&rig, "builder@agent", 1);
    let task_id = post_task(&rig).await;
    let a = register(&rig, "checker-a@agent", 2);
    let b = register(&rig, "checker-b@agent", 3);

    submit(&rig, task_id, &builder, agent_task_output(patch()).unwrap()).await;
    answer_latest(&rig, task_id, &[&a, &b], true).await;
    assert!(
        rig.payout.records().iter().all(|r| r.job_id != task_id),
        "a sampled pass waits for its confirmation"
    );
    assert_eq!(rig.state.jobs().get(task_id).unwrap().check_jobs.len(), 2);
    answer_latest(&rig, task_id, &[&a, &b], true).await;
    assert_eq!(settled(&rig, task_id).await, JobPhase::Completed);
}

/// Builds a task, has it checked once with `output` from the checker, and
/// returns the task id once it settles.
async fn checked_once(
    rig: &Rig,
    output: impl FnOnce(Uuid, &LocalIdentity) -> Vec<Content>,
) -> (Uuid, LocalIdentity) {
    let builder = register(rig, "builder@agent", 1);
    let task_id = post_task(rig).await;
    let checker = register(rig, "checker@agent", 2);
    submit(rig, task_id, &builder, agent_task_output(patch()).unwrap()).await;
    let (check_id, _) = latest_check(rig, task_id);
    submit(rig, check_id, &checker, output(task_id, &checker)).await;
    settled(rig, task_id).await;
    (task_id, checker)
}

fn paid(rig: &Rig, task_id: Uuid) -> bool {
    rig.payout.records().iter().any(|r| r.job_id == task_id)
}

#[tokio::test]
async fn the_chain_counts_the_signed_votes_before_the_builder_is_paid() {
    let rounds = Arc::new(NoopVoteRounds::new());
    let rig = rig_rounds(rounds.clone()).await;
    let (task_id, checker) = checked_once(&rig, |t, c| signed_verdict(t, c, true)).await;

    assert_eq!(
        rig.state.jobs().get(task_id).unwrap().phase,
        JobPhase::Completed
    );
    assert!(paid(&rig, task_id));
    let asked = rounds.asked();
    assert_eq!(asked.len(), 1);
    let (round_task, patch_sha, votes) = &asked[0];
    assert_eq!(
        (*round_task, patch_sha.as_str()),
        (task_id, patch().patch_sha256.as_str())
    );
    assert_eq!(votes.len(), 1);
    assert_eq!(votes[0].voter, checker.agent_id().pubkey_base58());
    assert!(votes[0].passed);
    let round = rig.state.jobs().get(task_id).unwrap().vote_round.unwrap();
    assert_eq!(round.result, RoundResult::Passed);
}

#[tokio::test]
async fn a_round_that_counts_otherwise_stops_the_payment() {
    let rounds = Arc::new(NoopVoteRounds::new());
    rounds.answer(RoundResult::Failed);
    let rig = rig_rounds(rounds.clone()).await;
    let (task_id, _) = checked_once(&rig, |t, c| signed_verdict(t, c, true)).await;

    let task = rig.state.jobs().get(task_id).unwrap();
    assert_eq!(task.phase, JobPhase::Refunded);
    assert_eq!(
        task.refund_reason,
        Some(covenant_compute_protocol::RefundReason::CheckUnavailable),
        "a disagreement faults nobody"
    );
    assert!(!paid(&rig, task_id));
}

#[tokio::test]
async fn an_unavailable_round_leaves_the_coordinators_decision() {
    let rounds = Arc::new(NoopVoteRounds::new());
    rounds.fail("the rollup is down");
    let rig = rig_rounds(rounds.clone()).await;
    let (task_id, _) = checked_once(&rig, |t, c| signed_verdict(t, c, true)).await;

    let task = rig.state.jobs().get(task_id).unwrap();
    assert_eq!(task.phase, JobPhase::Completed);
    assert!(task.vote_round.is_none());
    assert!(paid(&rig, task_id));
}

#[tokio::test]
async fn only_votes_signed_by_their_own_checker_go_to_a_round() {
    let rounds = Arc::new(NoopVoteRounds::new());
    let rig = rig_rounds(rounds.clone()).await;
    let (unsigned, _) = checked_once(&rig, |t, _| verdict(t, &patch().patch_sha256, 0)).await;
    assert!(paid(&rig, unsigned));

    let rig = rig_rounds(rounds.clone()).await;
    let stranger = LocalIdentity::generate("stranger@agent");
    let (forged, _) = checked_once(&rig, |t, _| signed_verdict(t, &stranger, true)).await;
    assert!(paid(&rig, forged));

    assert!(
        rounds.asked().is_empty(),
        "neither an unsigned vote nor one signed by another key reaches the chain"
    );
}

fn run(command: &str, exit_code: i32) -> CommandOutcome {
    CommandOutcome {
        command: command.into(),
        exit_code,
        duration_ms: 200,
        timed_out: false,
        output_tail: String::new(),
    }
}

/// A `code.tests` task whose hidden checks are the fix, handed over, built
/// and checked once with `verdict`.
async fn tests_task_checked_with(
    rig: &Rig,
    verdict: impl FnOnce(Uuid) -> AgentCheckVerdict,
) -> Uuid {
    let fix = covenant_compute_protocol::HiddenChecks {
        files: vec![covenant_compute_protocol::HiddenFile {
            path: "slugify.py".into(),
            content_b64: base64::engine::general_purpose::STANDARD.encode("def slugify(t): ...\n"),
        }],
        commands: vec![],
    };
    let mut tests = spec();
    tests.acceptance.skill = AgentSkill::CodeTests;
    tests.acceptance.protected_paths = vec!["slugify.py".into()];
    tests.acceptance.hidden_sha256 = Some(fix.digest());

    let builder = register(rig, "builder@agent", 1);
    let task_id = post_task_with(rig, tests).await;
    let checker = register(rig, "checker@agent", 2);
    submit(rig, task_id, &builder, agent_task_output(patch()).unwrap()).await;

    let url = format!("{}/federation/jobs/{task_id}/hidden", rig.url);
    let no_fix = covenant_compute_protocol::HiddenChecks {
        files: vec![],
        commands: vec![COMMAND.into()],
    };
    let refused = rig.http.post(&url).json(&no_fix).send().await.unwrap();
    assert_eq!(
        refused.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "code.tests is judged against a fix, so checks without one are refused"
    );
    let taken = rig.http.post(&url).json(&fix).send().await.unwrap();
    assert_eq!(taken.status(), reqwest::StatusCode::OK);

    let mut ordered = None;
    for _ in 0..100 {
        if let Some(id) = rig.state.jobs().get(task_id).unwrap().check_jobs.last() {
            ordered = Some(*id);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let check_id = ordered.expect("the fix orders the check");
    let check = rig.state.jobs().get(check_id).unwrap();
    let ordered_spec = parse_agent_check(&check.envelope.payload.input).unwrap();
    assert_eq!(ordered_spec.acceptance.skill, AgentSkill::CodeTests);
    assert_eq!(ordered_spec.hidden, Some(fix));

    let output = agent_check_output(verdict(task_id).sign_vote(&checker)).unwrap();
    submit(rig, check_id, &checker, output).await;
    covenant_compute_coordinator::agent::settle(&rig.state, task_id).await;
    task_id
}

#[tokio::test]
async fn tests_that_catch_the_bug_are_paid() {
    let rig = rig().await;
    let task_id = tests_task_checked_with(&rig, |task_id| {
        AgentCheckVerdict::catching(
            task_id,
            patch().patch_sha256,
            true,
            vec![],
            vec![run(COMMAND, 0)],
            vec![run(COMMAND, 1)],
            vec![run(COMMAND, 0)],
        )
    })
    .await;
    assert_eq!(settled(&rig, task_id).await, JobPhase::Completed);
    assert!(rig.payout.records().iter().any(|r| r.job_id == task_id));
}

#[tokio::test]
async fn a_tests_task_is_not_paid_on_a_change_verdict() {
    let rig = rig().await;
    // Every command passing is what code.change asks for; for code.tests it
    // means the new tests caught nothing.
    let task_id = tests_task_checked_with(&rig, |task_id| {
        AgentCheckVerdict::new(
            task_id,
            patch().patch_sha256,
            true,
            vec![],
            vec![run(COMMAND, 0)],
        )
    })
    .await;
    assert!(rig.payout.records().iter().all(|r| r.job_id != task_id));
    assert_eq!(
        rig.state.jobs().get(task_id).unwrap().phase,
        JobPhase::AwaitingCheck,
        "a verdict on the wrong skill is no verdict"
    );
}

#[tokio::test]
async fn a_passing_build_is_charged_what_it_spent_and_the_checks() {
    let rig = rig().await;
    let builder = register(&rig, "builder@agent", 1);
    let task_id = post_task(&rig).await;
    let checker = register(&rig, "checker@agent", 2);
    submit(
        &rig,
        task_id,
        &builder,
        agent_task_output(patch_spending(30_000)).unwrap(),
    )
    .await;
    let (check_id, _) = latest_check(&rig, task_id);
    submit(
        &rig,
        check_id,
        &checker,
        verdict(task_id, &patch().patch_sha256, 0),
    )
    .await;
    assert_eq!(settled(&rig, task_id).await, JobPhase::Completed);

    // 30,000 of model spend with a fifth on top, plus the 1,000 check.
    let paid: Vec<_> = rig
        .payout
        .records()
        .into_iter()
        .filter(|r| r.job_id == task_id)
        .collect();
    assert_eq!(paid[0].amount_micro_usdc, 36_000);
    assert_eq!(
        rig.state.escrow().settled_charge_micro_usdc(task_id),
        Some(37_000)
    );
    let task = rig.state.jobs().get(task_id).unwrap();
    assert_eq!(
        task.fee_micro_usdc, 1_000,
        "the check's part stays with the protocol"
    );
}

#[tokio::test]
async fn a_build_never_costs_more_than_the_offer() {
    let rig = rig().await;
    let builder = register(&rig, "builder@agent", 1);
    let task_id = post_task(&rig).await;
    let checker = register(&rig, "checker@agent", 2);
    submit(
        &rig,
        task_id,
        &builder,
        agent_task_output(patch_spending(1_000_000)).unwrap(),
    )
    .await;
    let (check_id, _) = latest_check(&rig, task_id);
    submit(
        &rig,
        check_id,
        &checker,
        verdict(task_id, &patch().patch_sha256, 0),
    )
    .await;
    assert_eq!(settled(&rig, task_id).await, JobPhase::Completed);
    assert_eq!(
        rig.state.escrow().settled_charge_micro_usdc(task_id),
        Some(TASK_PRICE)
    );
    let paid: Vec<_> = rig
        .payout
        .records()
        .into_iter()
        .filter(|r| r.job_id == task_id)
        .collect();
    assert_eq!(paid[0].amount_micro_usdc, TASK_PRICE - 1_000);
}

#[tokio::test]
async fn an_offer_too_small_for_a_build_is_refused() {
    let rig = rig().await;
    register(&rig, "builder@agent", 1);
    let buyer = LocalIdentity::generate("buyer@agent");
    let job_id = Uuid::new_v4();
    let payload = JobEnvelopePayload {
        job_id,
        buyer: buyer.agent_id(),
        kind: JobKind::AgentTask,
        capability_requirement: CapabilityRequirement {
            gpu_class: None,
            min_vram_gb: None,
            model_id: Some("claude-code".into()),
            kind: JobKind::AgentTask,
            max_duration_secs: 600,
            min_reputation_bps: None,
        },
        input: vec![agent_task_input(spec()).unwrap()],
        price_micro_usdc: 79_999,
        deadline_ms: 1_200_000,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, job_id.to_string()),
        issued_at_ms: epoch_ms(),
        referral_code: None,
        stream: false,
    };
    let envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
    let resp = rig
        .http
        .post(format!("{}/federation/jobs", rig.url))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(resp.text().await.unwrap().contains("at least 80000"));
}
