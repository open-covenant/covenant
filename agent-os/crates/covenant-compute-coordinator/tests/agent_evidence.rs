//! Authenticated HTTP export and offline verification of the same signed bytes.
use base64::Engine as _;
use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency, A2ATaskStatus};
use covenant_audit::InMemoryAuditLog;
use covenant_compute_buyer::verify_agent_evidence;
use covenant_compute_coordinator::{
    jobs::JobRecord, router, CoordinatorConfig, CoordinatorState, MockPayout, NoReputation,
};
use covenant_compute_protocol::*;
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use std::sync::Arc;
use uuid::Uuid;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
struct Fixture {
    buyer: LocalIdentity,
    builder: LocalIdentity,
    checker: LocalIdentity,
    check_buyer: LocalIdentity,
    bundle: AgentEvidenceBundle,
}
fn envelope(buyer: &LocalIdentity, kind: JobKind, input: Vec<Content>) -> SignedJobEnvelope {
    let id = Uuid::new_v4();
    SignedJobEnvelope::sign(
        JobEnvelopePayload {
            job_id: id,
            buyer: buyer.agent_id(),
            kind,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: Some(if kind == JobKind::AgentTask {
                    "claude-code".into()
                } else {
                    parse_agent_check(&input)
                        .unwrap()
                        .acceptance
                        .checker_model()
                }),
                kind,
                max_duration_secs: 120,
                min_reputation_bps: None,
            },
            input,
            price_micro_usdc: 100_000,
            deadline_ms: 600_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, id.to_string()),
            issued_at_ms: 1,
            referral_code: None,
            stream: false,
        },
        buyer,
    )
    .unwrap()
}
fn resign(row: &mut AgentJobEvidence, signer: &LocalIdentity) {
    let payload = WorkReceiptPayload {
        job_id: row.envelope.payload.job_id,
        operator: signer.agent_id(),
        job_hash_hex: sha256_hex(row.envelope.payload_json.as_bytes()),
        result_hash_hex: output_hash_hex(row.output.as_ref().unwrap()),
        meter: JobMeter {
            wall_ms: 42,
            tokens_in: None,
            tokens_out: None,
            gpu_seconds: None,
            finish_reason: None,
        },
        price_micro_usdc: 100,
        status: A2ATaskStatus::Ok,
        executed_at_ms: 2,
        node_audit_root_hex: "cc".repeat(32),
    };
    row.receipt = Some(SignedWorkReceipt::sign(payload, signer).unwrap());
}
fn row(
    envelope: SignedJobEnvelope,
    output: Vec<Content>,
    operator: &LocalIdentity,
) -> AgentJobEvidence {
    let mut row = AgentJobEvidence {
        envelope,
        output: Some(output),
        receipt: None,
        coordinator: AgentEvidenceMetadata {
            assigned_operator: operator.agent_id().pubkey_base58(),
            phase: "completed".into(),
            charged_micro_usdc: Some(100),
            payout: None,
        },
    };
    resign(&mut row, operator);
    row
}
fn fixture() -> Fixture {
    let buyer = LocalIdentity::generate("buyer@evidence");
    let builder = LocalIdentity::generate("builder@evidence");
    let checker = LocalIdentity::generate("checker@evidence");
    let check_buyer = LocalIdentity::generate("check-order@evidence");
    let acceptance = AcceptanceSpec {
        kani: None,
        skill: AgentSkill::CodeChange,
        image: "rust:test".into(),
        setup: vec![],
        commands: vec!["cargo test".into()],
        timeout_secs: 120,
        protected_paths: vec![],
        hidden_sha256: None,
    };
    let repo = RepoSource::Bundle {
        bundle_b64: "QUJD".into(),
        commit: COMMIT.into(),
    };
    let task = envelope(
        &buyer,
        JobKind::AgentTask,
        vec![agent_task_input(AgentTaskSpec {
            task: "write a patch".into(),
            repo: repo.clone(),
            acceptance: acceptance.clone(),
            runtime: AgentRuntime::ClaudeCode,
            model: None,
            reproduction: None,
        })
        .unwrap()],
    );
    let patch = b"PRIVATE_PATCH";
    let patch_sha256 = sha256_hex(patch);
    let patch_b64 = base64::engine::general_purpose::STANDARD.encode(patch);
    let check = envelope(
        &check_buyer,
        JobKind::AgentCheck,
        vec![agent_check_input(AgentCheckSpec {
            task_job_id: task.payload.job_id,
            repo,
            acceptance,
            patch_b64: patch_b64.clone(),
            patch_sha256: patch_sha256.clone(),
            hidden: None,
            reproduction: None,
        })
        .unwrap()],
    );
    let verdict = AgentCheckVerdict::new(
        task.payload.job_id,
        patch_sha256.clone(),
        true,
        vec![],
        vec![CommandOutcome {
            command: "cargo test".into(),
            exit_code: 0,
            duration_ms: 42,
            timed_out: false,
            output_tail: "ok".into(),
        }],
    )
    .sign_vote(&checker);
    let mut output = agent_check_output(verdict).unwrap();
    output.push(Content::json(
        serde_json::json!({"future_proof_report":{"log":"COMPLETE_PRIVATE_LOG"}}),
    ));
    let parent = row(
        task,
        agent_task_output(AgentTaskOutput {
            base_commit: COMMIT.into(),
            patch_b64,
            patch_sha256,
            files_changed: 1,
            summary: "done".into(),
            model: None,
            spend_micro_usd: 100,
            guard_run_id: None,
        })
        .unwrap(),
        &builder,
    );
    let child = row(check, output, &checker);
    Fixture {
        buyer,
        builder,
        checker,
        check_buyer,
        bundle: AgentEvidenceBundle {
            schema_version: 1,
            task: parent,
            checks: vec![child],
        },
    }
}
fn verify(
    f: &Fixture,
) -> Result<covenant_compute_buyer::VerifiedAgentEvidence, covenant_compute_buyer::BuyerError> {
    verify_agent_evidence(
        f.bundle.clone(),
        f.bundle.task.envelope.payload.job_id,
        &f.buyer.agent_id().pubkey_base58(),
    )
}
fn change_check(f: &mut Fixture, change: impl FnOnce(&mut AgentCheckSpec)) {
    let child = &mut f.bundle.checks[0];
    let mut spec = parse_agent_check(&child.envelope.payload.input).unwrap();
    change(&mut spec);
    child.envelope.payload.capability_requirement.model_id = Some(spec.acceptance.checker_model());
    child.envelope.payload.input = vec![Content::json(serde_json::json!({"agent_check":spec}))];
    child.envelope =
        SignedJobEnvelope::sign(child.envelope.payload.clone(), &f.check_buyer).unwrap();
    resign(child, &f.checker);
}

#[test]
fn agent_evidence_verifies_complete_bytes_and_preserves_extra_content() {
    let f = fixture();
    let v = verify(&f).unwrap();
    assert!(v.final_patch_accepted && v.checks[0].receipt_verified && v.checks[0].vote_verified);
    assert!(!v.inventory_completeness_verified && !v.settlement_verified);
    assert_eq!(v.evidence.checks[0].output, f.bundle.checks[0].output);
    assert_eq!(
        v.evidence.checks[0].receipt.as_ref().unwrap().receipt_json,
        f.bundle.checks[0].receipt.as_ref().unwrap().receipt_json
    );
}
#[test]
fn agent_evidence_rejects_signature_and_output_tampering() {
    for case in 0..7 {
        let mut f = fixture();
        let child = &mut f.bundle.checks[0];
        match case {
            0 => child.envelope.payload.price_micro_usdc += 1,
            1 => child.receipt.as_mut().unwrap().receipt.price_micro_usdc += 1,
            2 => child
                .output
                .as_mut()
                .unwrap()
                .push(Content::text("injected report")),
            3 => child.output.as_mut().unwrap()[1] = Content::text("replacement log"),
            4 => child.coordinator.assigned_operator = f.builder.agent_id().pubkey_base58(),
            5 => child.output = None,
            _ => child.receipt = None,
        }
        assert!(verify(&f).is_err(), "case {case}");
    }
}
#[test]
fn agent_evidence_rejects_signed_contract_substitution() {
    for case in 0..7 {
        let mut f = fixture();
        change_check(&mut f, |s| match case {
            0 => s.task_job_id = Uuid::new_v4(),
            1 => {
                s.repo = RepoSource::Bundle {
                    bundle_b64: "QUJD".into(),
                    commit: "a".repeat(40),
                }
            }
            2 => s.acceptance.commands = vec!["true".into()],
            3 => s.acceptance.image = "different:image".into(),
            4 => s.acceptance.timeout_secs += 1,
            5 => s.patch_b64 = "QUJD".into(),
            _ => s.patch_sha256 = "a".repeat(64),
        });
        assert!(verify(&f).is_err(), "case {case}");
    }
}
#[test]
fn agent_evidence_rejects_signed_wrong_job_hash_price_vote_or_commands() {
    for case in 0..5 {
        let mut f = fixture();
        let child = &mut f.bundle.checks[0];
        if case < 2 {
            let mut payload = child.receipt.as_ref().unwrap().receipt.clone();
            if case == 0 {
                payload.job_hash_hex = "a".repeat(64);
            } else {
                payload.price_micro_usdc = 100_001;
            }
            child.receipt = Some(SignedWorkReceipt::sign(payload, &f.checker).unwrap());
        } else {
            let mut v = parse_agent_check_verdict(child.output.as_ref().unwrap()).unwrap();
            match case {
                2 => v.vote_signature_b58 = Some("1".repeat(88)),
                3 => v.commands[0].command = "true".into(),
                _ => v.task_job_id = Uuid::new_v4(),
            }
            child.output.as_mut().unwrap()[0] =
                Content::json(serde_json::json!({"agent_check_verdict":v}));
            resign(child, &f.checker);
        }
        assert!(verify(&f).is_err(), "case {case}");
    }
}
#[test]
fn agent_evidence_keeps_old_failed_and_unserved_attempts() {
    let mut f = fixture();
    let mut old = f.bundle.checks[0].clone();
    old.envelope.payload.job_id = Uuid::new_v4();
    let mut check = parse_agent_check(&old.envelope.payload.input).unwrap();
    check.patch_b64 = "QUJD".into();
    check.patch_sha256 = sha256_hex(b"ABC");
    old.envelope.payload.input = vec![agent_check_input(check.clone()).unwrap()];
    old.envelope = SignedJobEnvelope::sign(old.envelope.payload, &f.check_buyer).unwrap();
    old.output = Some(
        agent_check_output(AgentCheckVerdict::new(
            check.task_job_id,
            check.patch_sha256,
            true,
            vec![],
            vec![CommandOutcome {
                command: "cargo test".into(),
                exit_code: 1,
                duration_ms: 1,
                timed_out: false,
                output_tail: "failure".into(),
            }],
        ))
        .unwrap(),
    );
    resign(&mut old, &f.checker);
    let mut unavailable = old.clone();
    unavailable.envelope.payload.job_id = Uuid::new_v4();
    unavailable.envelope =
        SignedJobEnvelope::sign(unavailable.envelope.payload, &f.check_buyer).unwrap();
    unavailable.receipt = None;
    unavailable.output = None;
    let mut failed = unavailable.clone();
    failed.envelope.payload.job_id = Uuid::new_v4();
    failed.envelope = SignedJobEnvelope::sign(failed.envelope.payload, &f.check_buyer).unwrap();
    failed.output = Some(vec![Content::text("engine failed")]);
    resign(&mut failed, &f.checker);
    let mut p = failed.receipt.take().unwrap().receipt;
    p.status = A2ATaskStatus::Error;
    failed.receipt = Some(SignedWorkReceipt::sign(p, &f.checker).unwrap());
    f.bundle.checks.splice(0..0, [old, unavailable, failed]);
    let v = verify(&f).unwrap();
    assert_eq!(v.checks.len(), 4);
    assert!(!v.checks[0].matches_final_patch);
    assert!(!v.checks[1].receipt_verified);
    assert!(v.checks[2].receipt_verified);
    assert_eq!(v.checks[2].execution_status, Some(A2ATaskStatus::Error));
    assert!(v.checks[2].verdict.is_none());
    f.bundle.checks.pop();
    assert!(verify(&f).is_err());
}

fn record(row: &AgentJobEvidence, parent: Option<Uuid>, children: Vec<Uuid>) -> JobRecord {
    let hold = EscrowHoldAttestation::sign(
        row.envelope.payload.job_id,
        100_000,
        FundingSource::Bootstrap,
        1,
        &LocalIdentity::generate("coordinator@evidence"),
    )
    .unwrap();
    serde_json::from_value(serde_json::json!({
        "operator_pubkey_b58": row.coordinator.assigned_operator,
        "envelope": row.envelope, "escrow_hold": hold, "phase": row.coordinator.phase,
        "receipt": row.receipt, "output": row.output, "checks_task": parent, "check_jobs": children
    }))
    .unwrap()
}
async fn server(f: &Fixture, phase: &str, broken: bool) -> (String, tokio::task::JoinHandle<()>) {
    let state = CoordinatorState::new(
        LocalIdentity::generate("coordinator@evidence"),
        CoordinatorConfig::default(),
        Arc::new(NoReputation),
        Arc::new(MockPayout::new()),
        Arc::new(InMemoryAuditLog::new()),
    );
    let id = f.bundle.task.envelope.payload.job_id;
    let child_id = f.bundle.checks[0].envelope.payload.job_id;
    let mut parent = record(&f.bundle.task, None, vec![child_id]);
    parent.phase = serde_json::from_value(serde_json::json!(phase)).unwrap();
    state.jobs().insert(id, parent).unwrap();
    state
        .jobs()
        .insert(
            child_id,
            record(
                &f.bundle.checks[0],
                Some(if broken { Uuid::new_v4() } else { id }),
                vec![],
            ),
        )
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        axum::serve(listener, router(state)).await.unwrap();
    });
    (base, handle)
}
fn signed_get(
    client: &reqwest::Client,
    url: &str,
    path: &str,
    key: &LocalIdentity,
) -> reqwest::RequestBuilder {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    client
        .get(url)
        .header(READ_SIGNED_AT_HEADER, now.to_string())
        .header(READ_SIGNATURE_HEADER, sign_read(key, path, now).unwrap())
}
#[tokio::test]
async fn agent_evidence_http_auth_and_paid_gate() {
    let f = fixture();
    let id = f.bundle.task.envelope.payload.job_id;
    let client = reqwest::Client::new();
    let path = format!("/federation/jobs/{id}/evidence");
    for phase in ["completed", "awaiting_check", "failed", "refunded"] {
        let (base, handle) = server(&f, phase, false).await;
        let url = format!("{base}{path}");
        for request in [
            client.get(&url),
            signed_get(&client, &url, &path, &f.checker),
            signed_get(
                &client,
                &url,
                &format!("/federation/jobs/{id}/receipt"),
                &f.buyer,
            ),
        ] {
            let r = request.send().await.unwrap();
            assert_eq!(r.status(), 401);
            assert!(!r.text().await.unwrap().contains("PRIVATE"));
        }
        let r = signed_get(&client, &url, &path, &f.buyer)
            .send()
            .await
            .unwrap();
        if phase == "completed" {
            assert_eq!(r.status(), 200);
            let e: AgentEvidenceBundle = r.json().await.unwrap();
            let verified =
                verify_agent_evidence(e, id, &f.buyer.agent_id().pubkey_base58()).unwrap();
            assert_eq!(
                verified.evidence.checks[0].output,
                f.bundle.checks[0].output
            );
            let config = covenant_compute_buyer::BuyerConfig {
                coordinator_url: base.clone(),
                poll_interval: std::time::Duration::from_millis(1),
                referral_code: None,
                rpc_url: None,
            };
            let fetched =
                covenant_compute_buyer::fetch_agent_evidence(&client, &config, &f.buyer, id)
                    .await
                    .unwrap();
            assert!(fetched.final_patch_accepted);
            assert_eq!(
                fetched.evidence.checks[0].envelope.payload_json,
                f.bundle.checks[0].envelope.payload_json
            );
        } else {
            assert_eq!(r.status(), 409);
            assert!(!r.text().await.unwrap().contains("PRIVATE"));
        }
        handle.abort();
    }
}
#[tokio::test]
async fn agent_evidence_http_corrupt_cross_parent_link_fails_closed() {
    let f = fixture();
    let id = f.bundle.task.envelope.payload.job_id;
    let (base, handle) = server(&f, "completed", true).await;
    let path = format!("/federation/jobs/{id}/evidence");
    let r = signed_get(
        &reqwest::Client::new(),
        &format!("{base}{path}"),
        &path,
        &f.buyer,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 500);
    assert!(!r.text().await.unwrap().contains("PRIVATE"));
    handle.abort();
}

#[test]
fn agent_evidence_rejects_missing_or_wrong_job_kani_report() {
    for with_wrong_job in [false, true] {
        let mut f = fixture();
        let mut spec = parse_agent_task(&f.bundle.task.envelope.payload.input).unwrap();
        spec.acceptance.commands.clear();
        spec.acceptance.image = format!("sha256:{}", "a".repeat(64));
        spec.acceptance.kani = Some(KaniCheckSpec {
            plan: KaniPlan {
                schema: "covenant.kani.plan.v1".into(),
                harness: "proof".into(),
                manifest_path: "Cargo.toml".into(),
                features: vec![],
                tests: false,
                timeout_secs: 60,
                unwind: None,
                expected_outcome: "proven".into(),
                expected_assertion: None,
                required_covers: vec!["reached".into()],
            },
            memory_mib: 2048,
            cpus: 1,
            scratch_mib: 256,
            check_price_micro_usdc: 50_000,
        });
        f.bundle.task.envelope.payload.input = vec![agent_task_input(spec.clone()).unwrap()];
        f.bundle.task.envelope =
            SignedJobEnvelope::sign(f.bundle.task.envelope.payload.clone(), &f.buyer).unwrap();
        resign(&mut f.bundle.task, &f.builder);
        change_check(&mut f, |c| c.acceptance = spec.acceptance.clone());
        if with_wrong_job {
            f.bundle.checks[0]
                .output
                .as_mut()
                .unwrap()
                .push(Content::json(serde_json::json!({
                    "kani_execution": {"job_id": Uuid::new_v4().to_string()}
                })));
            resign(&mut f.bundle.checks[0], &f.checker);
        }
        assert!(verify(&f).is_err());
    }
}

#[test]
fn agent_evidence_rejects_duplicate_child_and_another_buyer() {
    let mut f = fixture();
    assert!(verify_agent_evidence(
        f.bundle.clone(),
        f.bundle.task.envelope.payload.job_id,
        &f.checker.agent_id().pubkey_base58()
    )
    .is_err());
    f.bundle.checks.push(f.bundle.checks[0].clone());
    assert!(verify(&f).is_err());
}
