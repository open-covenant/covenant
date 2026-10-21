use super::*;
use base64::Engine as _;
use covenant_compute_attestation::binding::GuestEvidence;
use covenant_compute_protocol::{
    CapabilityRequirement, EscrowHoldAttestation, FundingSource, JobEnvelopePayload, LeaseTerms,
    SignedJobEnvelope,
};
use covenant_identity::LocalIdentity;
fn fixture() -> (
    Arc<CoordinatorState>,
    LocalIdentity,
    LocalIdentity,
    Uuid,
    u64,
) {
    let buyer = LocalIdentity::generate("buyer@hardware");
    let operator = LocalIdentity::generate("operator@hardware");
    let now = epoch_ms();
    let id = Uuid::new_v4();
    let state = Arc::new(CoordinatorState::new(
        LocalIdentity::generate("coordinator@hardware"),
        crate::CoordinatorConfig::default(),
        Arc::new(crate::reputation::NoReputation),
        Arc::new(crate::MockPayout::new()),
        Arc::new(covenant_audit::InMemoryAuditLog::new()),
    ));
    let envelope = SignedJobEnvelope::sign(
        JobEnvelopePayload {
            job_id: id,
            buyer: buyer.agent_id(),
            kind: JobKind::LeaseSession,
            capability_requirement: CapabilityRequirement {
                inference_tariff: None,
                gpu_class: None,
                min_vram_gb: None,
                model_id: None,
                kind: JobKind::LeaseSession,
                max_duration_secs: 600,
                min_reputation_bps: None,
            },
            input: vec![covenant_compute_protocol::lease_input(LeaseTerms {
                max_duration_secs: 600,
                rate_micro_usdc_per_sec: 1,
                client_public_key: None,
            })
            .unwrap()],
            price_micro_usdc: 600,
            deadline_ms: 900_000,
            idempotency: covenant_a2a::A2AIdempotency::new(
                covenant_a2a::A2ADuplicateSafety::Idempotent,
                "hardware-fixture",
            ),
            issued_at_ms: now,
            referral_code: None,
            stream: false,
        },
        &buyer,
    )
    .unwrap();
    let hold =
        EscrowHoldAttestation::sign(id, 600, FundingSource::Organic, now, &operator).unwrap();
    let mut key = vec![0, 0, 0, 11];
    key.extend_from_slice(b"ssh-ed25519");
    key.extend_from_slice(&[0, 0, 0, 32]);
    key.extend_from_slice(&[7; 32]);
    let record=serde_json::from_value(serde_json::json!({"operator_pubkey_b58":operator.agent_id().pubkey_base58(),"envelope":envelope,"escrow_hold":hold,"phase":"accepted","receipt":null,"output":null,"fee_micro_usdc":0,"accepted_at_ms":now,"lease_access":{"job_id":id,"endpoint":"ssh://root@gpu.example:2222","ready_at_ms":now,"ssh":{"host":"gpu.example","port":2222,"username":"root","lease_generation":"vm-1","observed_host_keys":[{"algorithm":"ssh-ed25519","key_base64":base64::engine::general_purpose::STANDARD.encode(key)}]}}})).unwrap();
    state.jobs().insert(id, record).unwrap();
    (state, buyer, operator, id, now)
}
fn bad_evidence(challenge: HardwareChallenge) -> HardwareEvidence {
    HardwareEvidence {
        challenge,
        guest: GuestEvidence::SevSnp {
            report_base64: "AA==".into(),
            certificate_chain_base64: Vec::new(),
        },
        gpu_cc: None,
    }
}
#[tokio::test]
async fn evidence_is_one_shot_durable_and_never_releases_a_stronger_floor() {
    let (state, buyer, operator, id, now) = fixture();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("hardware.json");
    let pin = "ab".repeat(32);
    let mut service = HardwareService::open(state.clone(), &path, pin.clone()).unwrap();
    service.clock = Arc::new(move || now + 2);
    let owner = buyer.agent_id().pubkey_base58();
    let op = operator.agent_id().pubkey_base58();
    assert!(service.challenge(id, "stranger", 0, now).is_err());
    let challenge = service.challenge(id, &owner, 0, now).unwrap();
    assert_eq!(challenge, service.challenge(id, &op, 0, now + 1).unwrap());
    assert!(service
        .submit(id, &owner, bad_evidence(challenge.clone()), now + 1)
        .await
        .is_err());
    assert!(!service.book.lock().data.rows[&id].consumed);
    assert!(service
        .submit(id, &op, bad_evidence(challenge.clone()), now + 2)
        .await
        .is_err());
    assert!(service.book.lock().data.rows[&id].consumed);
    assert!(service
        .require_floor(id, &owner, TrustClass::Attested, now + 3)
        .is_err());
    drop(service);
    let mut service = HardwareService::open(state.clone(), &path, pin.clone()).unwrap();
    service.clock = Arc::new(move || now + 3);
    assert!(service
        .submit(id, &op, bad_evidence(challenge), now + 3)
        .await
        .unwrap_err()
        .to_string()
        .contains("consumed"));
    assert!(HardwareService::open(state.clone(), &path, pin.clone()).is_err());
    drop(service);
    assert!(HardwareService::open(state.clone(), &path, "cd".repeat(32)).is_err());
    std::fs::write(&path, b"{broken").unwrap();
    assert!(HardwareService::open(state, &path, pin).is_err());
}
#[test]
fn challenges_bind_the_current_lease_generation_endpoint_key_and_paid_window() {
    let (state, buyer, _, id, now) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let service =
        HardwareService::open(state.clone(), &dir.path().join("book"), "ab".repeat(32)).unwrap();
    let owner = buyer.agent_id().pubkey_base58();
    let challenge = service.challenge(id, &owner, 0, now).unwrap();
    service.match_live_scope(&challenge.scope, now).unwrap();
    let mut changed = challenge.scope.clone();
    changed.lease_generation = "vm-2".into();
    assert!(service.match_live_scope(&changed, now).is_err());
    changed = challenge.scope.clone();
    changed.host = "other.example".into();
    assert!(service.match_live_scope(&changed, now).is_err());
    assert!(service.challenge(id, &owner, 0, now - 1).is_err());
    assert!(service.challenge(id, &owner, 1, now).is_err());
    assert!(service.challenge(id, &owner, 0, now + 600_000).is_err());
    state.jobs().request_lease_close(id, now + 1).unwrap();
    assert!(service.match_live_scope(&challenge.scope, now + 1).is_err());
}
#[test]
fn signed_http_authentication_cannot_be_replayed_for_a_different_route() {
    let actor = LocalIdentity::generate("actor@hardware");
    let now = epoch_ms();
    let path = format!("/federation/hardware/{}/challenge", Uuid::new_v4());
    let body = b"{}";
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-covenant-hardware-actor",
        actor.agent_id().pubkey_base58().parse().unwrap(),
    );
    headers.insert(
        "x-covenant-hardware-signed-at",
        now.to_string().parse().unwrap(),
    );
    headers.insert(
        "x-covenant-hardware-signature",
        binding::sign_request(&actor, "POST", &path, body, now)
            .unwrap()
            .parse()
            .unwrap(),
    );
    assert!(authenticate(&headers, "POST", &path, body, now).is_ok());
    assert!(authenticate(&headers, "POST", &path, b"{\"host_key_index\":1}", now).is_err());
    assert!(authenticate(&headers, "POST", &path, body, now + 30_001).is_err());
    assert!(authenticate(&headers, "GET", &path, body, now).is_err());
    assert!(authenticate(&headers, "POST", &(path + "/other"), body, now).is_err());
}

#[tokio::test]
async fn overload_does_not_consume_or_queue_a_challenge() {
    let (state, buyer, operator, id, now) = fixture();
    let directory = tempfile::tempdir().unwrap();
    let mut service =
        HardwareService::open(state, &directory.path().join("hardware"), "ab".repeat(32)).unwrap();
    service.clock = Arc::new(move || now);
    let challenge = service
        .challenge(id, &buyer.agent_id().pubkey_base58(), 0, now)
        .unwrap();
    let busy = service
        .verify_slots
        .clone()
        .acquire_many_owned(2)
        .await
        .unwrap();
    let error = service
        .submit(
            id,
            &operator.agent_id().pubkey_base58(),
            bad_evidence(challenge.clone()),
            now,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, HardwareError::Unavailable(_)));
    assert!(error.to_string().contains("capacity"));
    assert!(!service.book.lock().data.rows[&id].consumed);
    drop(busy);
    assert!(service
        .submit(
            id,
            &operator.agent_id().pubkey_base58(),
            bad_evidence(challenge),
            now,
        )
        .await
        .is_err());
    assert!(service.book.lock().data.rows[&id].consumed);
    assert_eq!(service.verify_slots.available_permits(), 2);
}

#[test]
fn verdict_reads_persist_clock_progress_and_refuse_rollback_after_restart() {
    let (state, buyer, _, id, now) = fixture();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("hardware");
    let pin = "ab".repeat(32);
    let service = HardwareService::open(state.clone(), &path, pin.clone()).unwrap();
    let owner = buyer.agent_id().pubkey_base58();
    service.challenge(id, &owner, 0, now).unwrap();
    // Even a read with no verified evidence records the observed clock.
    assert!(service.evidence(id, &owner, now + 100).is_err());
    assert!(service
        .evidence(id, &owner, now + 99)
        .unwrap_err()
        .to_string()
        .contains("backwards"));
    drop(service);
    let service = HardwareService::open(state, &path, pin).unwrap();
    assert!(service
        .challenge(id, &owner, 0, now + 99)
        .unwrap_err()
        .to_string()
        .contains("backwards"));
}

#[test]
fn completed_verification_cannot_extend_collection_or_lease_time() {
    let (state, buyer, _, id, now) = fixture();
    let directory = tempfile::tempdir().unwrap();
    let service = HardwareService::open(
        state.clone(),
        &directory.path().join("hardware"),
        "ab".repeat(32),
    )
    .unwrap();
    let challenge = service
        .challenge(id, &buyer.agent_id().pubkey_base58(), 0, now)
        .unwrap();
    // This synthetic verdict tests only the post-verification timing gate;
    // it is never treated as cryptographically verified evidence.
    let verdict = HardwareVerdict {
        phala_identity: None,
        challenge,
        evidence_sha256: "00".repeat(32),
        reference_sha256: binding::reference_digest(),
        guest_identity: "test-only".into(),
        gpu_identity: None,
        channel_key_fingerprint: "test-only".into(),
        evidence_class: TrustClass::Attested,
        served_class: TrustClass::Open,
        verifier_version: "test-only".into(),
        verified_at_ms: now + 1,
        expires_at_ms: now + binding::VERDICT_TTL_MS,
    };
    assert!(service.validate_completion(&verdict, now + 2).is_ok());
    assert!(service.validate_completion(&verdict, now).is_err());
    assert!(service
        .validate_completion(&verdict, now + binding::CHALLENGE_TTL_MS)
        .is_err());
    state.jobs().request_lease_close(id, now + 3).unwrap();
    assert!(service.validate_completion(&verdict, now + 4).is_err());
}

#[tokio::test]
async fn worker_rechecks_clock_and_challenge_before_vendor_verification() {
    let (state, buyer, operator, id, now) = fixture();
    let directory = tempfile::tempdir().unwrap();
    let mut service =
        HardwareService::open(state, &directory.path().join("hardware"), "ab".repeat(32)).unwrap();
    let owner = buyer.agent_id().pubkey_base58();
    let actor = operator.agent_id().pubkey_base58();
    let challenge = service.challenge(id, &owner, 0, now).unwrap();
    service.clock = Arc::new(move || now + binding::CHALLENGE_TTL_MS);
    let error = service
        .submit(id, &actor, bad_evidence(challenge), now)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("stale or invalid challenge"));
    assert!(!service.book.lock().data.rows[&id].consumed);

    let next = now + binding::CHALLENGE_TTL_MS + 1;
    let challenge = service.challenge(id, &owner, 0, next).unwrap();
    service.clock = Arc::new(move || next - 1);
    let error = service
        .submit(id, &actor, bad_evidence(challenge), next)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("clock moved backwards"));
    assert!(!service.book.lock().data.rows[&id].consumed);
}

#[test]
fn all_stronger_workspace_floors_remain_unreleased() {
    let (state, buyer, _, id, now) = fixture();
    let directory = tempfile::tempdir().unwrap();
    let service =
        HardwareService::open(state, &directory.path().join("hardware"), "ab".repeat(32)).unwrap();
    assert_eq!(binding::SERVED_TRUST_CEILING, TrustClass::Open);
    for floor in [
        TrustClass::Isolated,
        TrustClass::Attested,
        TrustClass::Confidential,
    ] {
        assert!(service
            .require_floor(id, &buyer.agent_id().pubkey_base58(), floor, now)
            .unwrap_err()
            .to_string()
            .contains("not released"));
    }
}

#[test]
fn challenges_refuse_legacy_or_mismatched_ssh_access_without_fallback() {
    let (state, buyer, _, id, now) = fixture();
    let directory = tempfile::tempdir().unwrap();
    let service = HardwareService::open(
        state.clone(),
        &directory.path().join("hardware"),
        "ab".repeat(32),
    )
    .unwrap();
    let original = state.jobs().get(id).unwrap();
    let owner = buyer.agent_id().pubkey_base58();
    for mode in 0..4 {
        let mut changed = original.clone();
        let access = changed.lease_access.as_mut().unwrap();
        match mode {
            0 => access.ssh = None,
            1 => access.job_id = Uuid::new_v4(),
            2 => access.ready_at_ms = now + 1,
            3 => access.ssh.as_mut().unwrap().observed_host_keys[0].key_base64 = "AA==".into(),
            _ => unreachable!(),
        }
        state.jobs().insert(id, changed).unwrap();
        assert!(
            service.challenge(id, &owner, 0, now).is_err(),
            "mode {mode}"
        );
        assert!(service.book.lock().data.rows.is_empty());
    }
    state.jobs().insert(id, original).unwrap();
    assert!(service.challenge(id, &owner, 0, now).is_ok());
}

#[tokio::test]
async fn hardware_http_rejects_unauthenticated_requests_without_state_changes() {
    let (state, _, _, id, _) = fixture();
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(
        HardwareService::open(state, &directory.path().join("hardware"), "ab".repeat(32)).unwrap(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = router(service.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    for (method, route) in [
        (reqwest::Method::POST, "challenge"),
        (reqwest::Method::POST, "evidence"),
        (reqwest::Method::GET, "verdict"),
    ] {
        let response = client
            .request(method, format!("{base}/federation/hardware/{id}/{route}"))
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(service.book.lock().data.rows.is_empty());
    }
    assert_eq!(service.verify_slots.available_permits(), 2);
    server.abort();
}

// Deliberately invalid vendor evidence: these rows exercise admission and
// worker timing only, and must never pass the production verifier.
fn cache_unverified_fixture(
    service: &HardwareService,
    challenge: HardwareChallenge,
    now: u64,
    expires_at_ms: u64,
) -> HardwareEvidence {
    let evidence = bad_evidence(challenge.clone());
    let verdict = HardwareVerdict {
        phala_identity: None,
        challenge: challenge.clone(),
        evidence_sha256: "00".repeat(32),
        reference_sha256: binding::reference_digest(),
        guest_identity: "test-only".into(),
        gpu_identity: None,
        channel_key_fingerprint: "test-only".into(),
        evidence_class: TrustClass::Attested,
        served_class: TrustClass::Open,
        verifier_version: "test-only".into(),
        verified_at_ms: now,
        expires_at_ms,
    };
    let mut book = service.book.lock();
    let row = book.data.rows.get_mut(&challenge.scope.job_id).unwrap();
    row.consumed = true;
    row.evidence = Some(evidence.clone());
    row.verdict = Some(verdict);
    book.save().unwrap();
    evidence
}

#[tokio::test]
async fn saturated_reads_challenges_and_cached_submits_never_schedule_or_touch_journal() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (state, buyer, operator, id, now) = fixture();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("hardware");
    let mut service = HardwareService::open(state, &path, "ab".repeat(32)).unwrap();
    let owner = buyer.agent_id().pubkey_base58();
    let actor = operator.agent_id().pubkey_base58();
    let challenge = service.challenge(id, &owner, 0, now).unwrap();
    let evidence = cache_unverified_fixture(&service, challenge, now, now + 1000);
    let worker_calls = Arc::new(AtomicUsize::new(0));
    let observed = worker_calls.clone();
    service.clock = Arc::new(move || {
        observed.fetch_add(1, Ordering::SeqCst);
        now + 1
    });
    let before = std::fs::read(&path).unwrap();
    let busy = service
        .verify_slots
        .clone()
        .acquire_many_owned(2)
        .await
        .unwrap();
    assert!(service
        .evidence_blocking(id, &owner, now + 1)
        .await
        .unwrap_err()
        .to_string()
        .contains("capacity"));
    assert!(service
        .challenge_blocking(id, &owner, 0, now + 1)
        .await
        .unwrap_err()
        .to_string()
        .contains("capacity"));
    assert!(service
        .submit(id, &actor, evidence, now + 1)
        .await
        .unwrap_err()
        .to_string()
        .contains("capacity"));
    assert_eq!(
        worker_calls.load(Ordering::SeqCst),
        0,
        "overload must fail before a blocking worker starts"
    );
    assert_eq!(service.book.lock().data.watermark_ms, now);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    drop(busy);
    // A correctly signed stranger still cannot enqueue work for this lease.
    assert!(service
        .evidence_blocking(id, "stranger", now + 1)
        .await
        .unwrap_err()
        .to_string()
        .contains("lease operator or buyer"));
    assert_eq!(worker_calls.load(Ordering::SeqCst), 0);
    assert_eq!(service.verify_slots.available_permits(), 2);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn cached_reads_use_worker_time_instead_of_request_arrival_time() {
    let (state, buyer, _, id, now) = fixture();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("hardware");
    let mut service = HardwareService::open(state, &path, "ab".repeat(32)).unwrap();
    let owner = buyer.agent_id().pubkey_base58();
    let challenge = service.challenge(id, &owner, 0, now).unwrap();
    cache_unverified_fixture(&service, challenge, now, now + 50);
    service.clock = Arc::new(move || now + 100);
    let error = service
        .evidence_blocking(id, &owner, now)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("verdict expired"),
        "must reject worker-time expiry before attempting the invalid vendor evidence: {error}"
    );
    assert_eq!(service.book.lock().data.watermark_ms, now + 100);
    assert_eq!(service.verify_slots.available_permits(), 2);
}

#[tokio::test]
async fn challenge_fresh_submit_and_cached_retry_run_on_blocking_workers() {
    let (state, buyer, operator, id, now) = fixture();
    let directory = tempfile::tempdir().unwrap();
    let mut service =
        HardwareService::open(state, &directory.path().join("hardware"), "ab".repeat(32)).unwrap();
    let requester_thread = std::thread::current().id();
    let worker_threads = Arc::new(Mutex::new(Vec::new()));
    let observed = worker_threads.clone();
    service.clock = Arc::new(move || {
        observed.lock().push(std::thread::current().id());
        now
    });
    let owner = buyer.agent_id().pubkey_base58();
    let actor = operator.agent_id().pubkey_base58();
    let challenge = service
        .challenge_blocking(id, &owner, 0, now)
        .await
        .unwrap();
    assert!(service
        .submit(id, &actor, bad_evidence(challenge.clone()), now)
        .await
        .unwrap_err()
        .to_string()
        .contains("certificate chain exceeds limits"));
    let evidence = cache_unverified_fixture(&service, challenge, now, now + 1000);
    assert!(service
        .submit(id, &actor, evidence, now)
        .await
        .unwrap_err()
        .to_string()
        .contains("certificate chain exceeds limits"));
    assert!(service
        .evidence_blocking(id, &owner, now)
        .await
        .unwrap_err()
        .to_string()
        .contains("certificate chain exceeds limits"));
    let observed = worker_threads.lock();
    assert!(observed.len() >= 4);
    assert!(
        observed.iter().all(|thread| *thread != requester_thread),
        "hardware filesystem and crypto workflow must leave the async requester thread"
    );
    assert_eq!(service.verify_slots.available_permits(), 2);
}

fn phala_fixture() -> (
    Arc<CoordinatorState>,
    LocalIdentity,
    LocalIdentity,
    Uuid,
    u64,
    covenant_compute_protocol::phala::PhalaLeaseSpec,
) {
    use covenant_compute_protocol::phala::{
        phala_lease_input, phala_workload, reviewed_phala_wrapper, PHALA_CPU_MODEL, PHALA_GATEWAY,
    };
    let (state, buyer, operator, id, now) = fixture();
    let mut record = state.jobs().get(id).unwrap();
    let mut payload = record.envelope.payload;
    payload.issued_at_ms = now - 60_000;
    payload.stream = true;
    payload.capability_requirement.gpu_class = Some("cpu".into());
    payload.capability_requirement.model_id = Some(PHALA_CPU_MODEL.into());
    let ssh = record.lease_access.as_mut().unwrap().ssh.as_mut().unwrap();
    payload.input = vec![covenant_compute_protocol::lease_input(LeaseTerms {
        max_duration_secs: 600,
        rate_micro_usdc_per_sec: 1,
        client_public_key: Some(format!(
            "ssh-ed25519 {}",
            ssh.observed_host_keys[0].key_base64
        )),
    })
    .unwrap()];
    let (_, spec) = phala_workload(
        &payload,
        &operator.agent_id().pubkey_base58(),
        &reviewed_phala_wrapper(),
        &format!("sha256:{}", "cd".repeat(32)),
    )
    .unwrap();
    payload.input.push(phala_lease_input(spec.clone()).unwrap());
    payload.validate_input().unwrap();
    record.envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
    ssh.host = format!("{}-1022.{PHALA_GATEWAY}", "12".repeat(20));
    ssh.port = 443;
    ssh.username = "covenant".into();
    ssh.transport = Some(covenant_compute_protocol::ssh::SshTransport::Tls);
    ssh.lease_generation = spec.lease_generation.clone();
    let endpoint = format!("ssh+tls://covenant@{}:443", ssh.host);
    record.lease_access.as_mut().unwrap().endpoint = endpoint;
    state.jobs().insert(id, record).unwrap();
    (state, buyer, operator, id, now, spec)
}

#[test]
fn phala_signed_expiry_caps_scope_view_and_both_meters_after_delayed_acceptance() {
    let (state, buyer, _, id, now, spec) = phala_fixture();
    let directory = tempfile::tempdir().unwrap();
    let global_pin = "ab".repeat(32);
    let service = HardwareService::open(
        state.clone(),
        &directory.path().join("hardware"),
        global_pin.clone(),
    )
    .unwrap();
    let challenge = service
        .challenge(id, &buyer.agent_id().pubkey_base58(), 0, now)
        .unwrap();
    assert_ne!(challenge.scope.workload_sha256, global_pin);
    assert_eq!(challenge.scope.workload_sha256, spec.workload_sha256);
    assert_eq!(challenge.scope.lease_expires_at_ms, now + 540_000);
    assert_eq!(
        challenge.scope.lease_expires_at_ms,
        spec.lease_expires_at_ms
    );
    assert!(service.scope(id, 0, spec.lease_expires_at_ms - 1).is_ok());
    assert!(service.scope(id, 0, spec.lease_expires_at_ms).is_err());

    let mut record = state.jobs().get(id).unwrap();
    assert_eq!(record.live_lease_elapsed_ms(now - 1), 0);
    assert_eq!(record.live_lease_elapsed_ms(now + 120_000), 120_000);
    let late_result = spec.lease_expires_at_ms + 120_000;
    // A delayed receipt or slow deletion cannot bill after the guest's signed
    // shutdown, even though the generic deadline still permits settlement.
    assert_eq!(record.live_lease_elapsed_ms(late_result), 540_000);
    assert_eq!(record.lease_elapsed_ms(late_result), 540_000);
    let terms = parse_lease_terms(&record.envelope.payload.input)
        .unwrap()
        .unwrap();
    assert_eq!(
        record.lease_charged_micro_usdc(&terms, late_result, None),
        540
    );
    let observation = crate::onchain_meter::observe_lease(&record, late_result).unwrap();
    assert_eq!(observation.elapsed_ms, 540_000);
    assert_eq!(observation.charged_micro_usdc, 540);

    record.metered_elapsed_ms = Some(17_000);
    record.phase = JobPhase::Completed;
    assert_eq!(record.lease_elapsed_ms(late_result), 17_000);
    assert_eq!(
        record.lease_charged_micro_usdc(&terms, late_result, None),
        17
    );
    record.metered_elapsed_ms = None;
    assert_eq!(
        record.lease_charged_micro_usdc(&terms, late_result, Some(19)),
        19
    );

    let (legacy_state, _, _, legacy_id, legacy_now) = fixture();
    let mut legacy = legacy_state.jobs().get(legacy_id).unwrap();
    assert_eq!(legacy.live_lease_elapsed_ms(legacy_now + 700_000), 700_000);
    legacy.metered_elapsed_ms = Some(23_000);
    legacy.phase = JobPhase::Completed;
    assert_eq!(legacy.lease_elapsed_ms(legacy_now + 700_000), 23_000);
}

#[test]
fn signed_lease_mode_has_no_operator_global_workload_fallback() {
    let (state, buyer, _, id, now) = fixture();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("hardware");
    let service = HardwareService::open_signed_leases(state.clone(), &path).unwrap();
    assert!(service
        .challenge(id, &buyer.agent_id().pubkey_base58(), 0, now)
        .is_err());
    service.book.lock().save().unwrap();
    drop(service);
    assert!(HardwareService::open(state, &path, "ab".repeat(32)).is_err());

    let (state, buyer, _, id, now, spec) = phala_fixture();
    let path = directory.path().join("signed");
    let service = HardwareService::open_signed_leases(state, &path).unwrap();
    let challenge = service
        .challenge(id, &buyer.agent_id().pubkey_base58(), 0, now)
        .unwrap();
    assert_eq!(challenge.scope.workload_sha256, spec.workload_sha256);
}

#[test]
fn phala_scope_rejects_unpinned_operator_or_wrong_generation_and_tls_endpoint() {
    let (state, _, _, id, now, _) = phala_fixture();
    let directory = tempfile::tempdir().unwrap();
    let service = HardwareService::open(
        state.clone(),
        &directory.path().join("hardware"),
        "ab".repeat(32),
    )
    .unwrap();
    let original = state.jobs().get(id).unwrap();
    for mode in 0..9 {
        let mut changed = original.clone();
        if mode == 0 {
            changed.operator_pubkey_b58 = LocalIdentity::generate("other@hardware")
                .agent_id()
                .pubkey_base58();
        } else {
            let ssh = changed.lease_access.as_mut().unwrap().ssh.as_mut().unwrap();
            match mode {
                1 => ssh.lease_generation.push_str("-replacement"),
                2 => ssh.transport = None,
                3 => ssh.port = 1022,
                4 => ssh.username = "root".into(),
                5 => ssh.host = format!("{}-1022.untrusted.example", "12".repeat(20)),
                6 => {
                    ssh.host = format!(
                        "{}-1022.{}",
                        "AB".repeat(20),
                        covenant_compute_protocol::phala::PHALA_GATEWAY
                    )
                }
                7 => {
                    ssh.host = format!(
                        "{}-1022.{}",
                        "12".repeat(19),
                        covenant_compute_protocol::phala::PHALA_GATEWAY
                    )
                }
                8 => ssh.observed_host_keys[0].key_base64 = "AA==".into(),
                _ => unreachable!(),
            }
        }
        state.jobs().insert(id, changed).unwrap();
        assert!(service.scope(id, 0, now).is_err(), "mode {mode}");
    }
    state.jobs().insert(id, original).unwrap();
    assert!(service.scope(id, 0, now).is_ok());
}

#[test]
fn phala_challenge_cannot_be_reused_with_changed_workload_transport_or_key() {
    let (state, buyer, _, id, now, _) = phala_fixture();
    let directory = tempfile::tempdir().unwrap();
    let service = HardwareService::open(
        state.clone(),
        &directory.path().join("hardware"),
        "ab".repeat(32),
    )
    .unwrap();
    let challenge = service
        .challenge(id, &buyer.agent_id().pubkey_base58(), 0, now)
        .unwrap();
    service.match_live_scope(&challenge.scope, now).unwrap();
    for mode in 0..6 {
        let mut altered = challenge.scope.clone();
        match mode {
            0 => altered.workload_sha256 = "ef".repeat(32),
            1 => altered.transport = None,
            2 => altered.lease_generation.push_str("-replacement"),
            3 => altered.lease_expires_at_ms += 1,
            4 => {
                let mut key = base64::engine::general_purpose::STANDARD
                    .decode(&altered.guest_channel_key.key_base64)
                    .unwrap();
                *key.last_mut().unwrap() ^= 1;
                altered.guest_channel_key.key_base64 =
                    base64::engine::general_purpose::STANDARD.encode(key);
                altered.guest_channel_key.validate().unwrap();
            }
            5 => {
                altered.host = format!(
                    "{}-1022.{}",
                    "34".repeat(20),
                    covenant_compute_protocol::phala::PHALA_GATEWAY
                )
            }
            _ => unreachable!(),
        }
        assert!(
            service.match_live_scope(&altered, now).is_err(),
            "mode {mode}"
        );
    }
    let mut record = state.jobs().get(id).unwrap();
    let covenant_mcp::Content::Json { value } = record.envelope.payload.input.last_mut().unwrap()
    else {
        unreachable!()
    };
    value["phala_lease"]["workload_sha256"] = "ef".repeat(32).into();
    record.envelope = SignedJobEnvelope::sign(record.envelope.payload, &buyer).unwrap();
    state.jobs().insert(id, record).unwrap();
    assert!(service.match_live_scope(&challenge.scope, now).is_err());
    assert_eq!(
        service.scope(id, 0, now).unwrap().workload_sha256,
        "ef".repeat(32)
    );
}

fn ephemeral_fixture() -> (
    Arc<CoordinatorState>,
    LocalIdentity,
    LocalIdentity,
    Uuid,
    u64,
    covenant_compute_protocol::phala::PhalaEphemeralLeaseSpec,
) {
    use covenant_compute_protocol::phala::*;
    let (state, buyer, operator, id, now, _) = phala_fixture();
    let mut record = state.jobs().get(id).unwrap();
    let mut payload = record.envelope.payload;
    payload.input.retain(|content| !matches!(content, covenant_mcp::Content::Json { value } if value.get("phala_lease").is_some()));
    payload.capability_requirement.model_id = Some(PHALA_EPHEMERAL_CPU_MODEL.into());
    payload.deadline_ms = u64::from(payload.capability_requirement.max_duration_secs) * 1_000
        + PHALA_EPHEMERAL_RESULT_SLACK_MS;
    let relay = PhalaRelayConfig {
        url: "https://relay.example.com".into(),
        session_id: id.to_string(),
    };
    let (_, spec) = phala_ephemeral_workload(
        &payload,
        &operator.agent_id().pubkey_base58(),
        &reviewed_phala_ephemeral_wrapper(),
        &format!("sha256:{}", "cd".repeat(32)),
        &relay,
    )
    .unwrap();
    payload
        .input
        .push(phala_ephemeral_lease_input(spec.clone()).unwrap());
    payload.validate_input().unwrap();
    record.envelope = SignedJobEnvelope::sign(payload, &buyer).unwrap();
    let ssh = record.lease_access.as_mut().unwrap().ssh.as_mut().unwrap();
    ssh.host = relay.host().unwrap().into();
    ssh.transport = Some(covenant_compute_protocol::ssh::SshTransport::OutboundRelay);
    ssh.lease_generation = spec.lease_generation.clone();
    record.lease_access.as_mut().unwrap().endpoint =
        "ssh+relay://covenant@relay.example.com:443".into();
    state.jobs().insert(id, record).unwrap();
    (state, buyer, operator, id, now, spec)
}

#[test]
fn ephemeral_profile_is_derived_from_signed_lease_and_caps_both_paid_meters() {
    let (state, buyer, _, id, now, spec) = ephemeral_fixture();
    let directory = tempfile::tempdir().unwrap();
    let service =
        HardwareService::open_signed_leases(state.clone(), &directory.path().join("hardware"))
            .unwrap();
    let challenge = service
        .challenge(id, &buyer.agent_id().pubkey_base58(), 0, now)
        .unwrap();
    assert_eq!(
        challenge.scope.profile,
        HardwareProfile::PhalaEphemeralDstack061
    );
    assert_eq!(challenge.scope.workload_sha256, spec.workload_sha256);
    assert_eq!(
        challenge.scope.lease_expires_at_ms,
        spec.lease_expires_at_ms
    );
    assert!(service.scope(id, 0, spec.lease_expires_at_ms).is_err());
    let record = state.jobs().get(id).unwrap();
    let late = spec.lease_expires_at_ms + 120_000;
    assert_eq!(record.live_lease_elapsed_ms(late), 540_000);
    let observation = crate::onchain_meter::observe_lease(&record, late).unwrap();
    assert_eq!(observation.elapsed_ms, 540_000);
    assert_eq!(observation.charged_micro_usdc, 540);
    let legacy = HardwareService::open(
        state.clone(),
        &directory.path().join("legacy"),
        "ab".repeat(32),
    )
    .unwrap();
    assert!(
        legacy.scope(id, 0, now).is_err(),
        "fixed-workload diagnostic mode cannot enable an ephemeral profile"
    );
    state.jobs().request_lease_close(id, now + 1).unwrap();
    assert!(service.match_live_scope(&challenge.scope, now + 1).is_err());
    let closed = state.jobs().get(id).unwrap();
    assert_eq!(
        closed.live_lease_elapsed_ms(late),
        1,
        "provider deletion latency must not be charged after buyer close"
    );
    assert_eq!(
        crate::onchain_meter::observe_lease(&closed, late)
            .unwrap()
            .elapsed_ms,
        1
    );
}

#[test]
fn ephemeral_scope_rejects_substituted_channel_operator_or_generation() {
    let (state, _, _, id, now, _) = ephemeral_fixture();
    let directory = tempfile::tempdir().unwrap();
    let service =
        HardwareService::open_signed_leases(state.clone(), &directory.path().join("hardware"))
            .unwrap();
    let original = state.jobs().get(id).unwrap();
    for case in 0..6 {
        let mut changed = original.clone();
        let ssh = changed.lease_access.as_mut().unwrap().ssh.as_mut().unwrap();
        match case {
            0 => ssh.transport = Some(covenant_compute_protocol::ssh::SshTransport::Tls),
            1 => ssh.host = "other.example.com".into(),
            2 => ssh.lease_generation.push_str("-replacement"),
            3 => ssh
                .observed_host_keys
                .push(ssh.observed_host_keys[0].clone()),
            4 => ssh.username = "root".into(),
            _ => {
                changed.operator_pubkey_b58 = LocalIdentity::generate("wrong@operator")
                    .agent_id()
                    .pubkey_base58()
            }
        }
        state.jobs().insert(id, changed).unwrap();
        assert!(service.scope(id, 0, now).is_err(), "mutation {case}");
    }
    state.jobs().insert(id, original).unwrap();
    assert!(service.scope(id, 0, now).is_ok());
}

#[tokio::test]
async fn ephemeral_remote_evidence_cannot_downgrade_the_selected_profile() {
    let (state, _, operator, id, now, _) = ephemeral_fixture();
    let directory = tempfile::tempdir().unwrap();
    let mut service =
        HardwareService::open_signed_leases(state, &directory.path().join("hardware")).unwrap();
    service.clock = Arc::new(move || now);
    let actor = operator.agent_id().pubkey_base58();
    let challenge = service.challenge(id, &actor, 0, now).unwrap();
    let mut evidence = bad_evidence(challenge);
    evidence.challenge.scope.profile = HardwareProfile::Standard;
    assert!(service
        .submit(id, &actor, evidence, now)
        .await
        .unwrap_err()
        .to_string()
        .contains("current lease/endpoint/key"));
    assert!(!service.book.lock().data.rows[&id].consumed);
}
