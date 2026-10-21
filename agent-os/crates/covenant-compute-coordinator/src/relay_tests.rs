use super::*;
use covenant_compute_protocol::{
    phala::{phala_ephemeral_lease_input, PhalaEphemeralLeaseSpec, PHALA_EPHEMERAL_CPU_MODEL},
    CapabilityRequirement, EscrowHoldAttestation, FundingSource, JobEnvelopePayload, JobKind,
    LeaseTerms, SignedJobEnvelope,
};
use covenant_identity::LocalIdentity;

fn fixture() -> (
    tempfile::TempDir,
    RelayService,
    LocalIdentity,
    LocalIdentity,
    Uuid,
) {
    let directory = tempfile::tempdir().unwrap();
    let buyer = LocalIdentity::generate("buyer@relay");
    let operator = LocalIdentity::generate("operator@relay");
    let now = epoch_ms();
    let job = Uuid::new_v4();
    let endpoint = "https://relay.example.com";
    let state = Arc::new(CoordinatorState::new(
        LocalIdentity::generate("coordinator@relay"),
        crate::CoordinatorConfig::default(),
        Arc::new(crate::reputation::NoReputation),
        Arc::new(crate::MockPayout::new()),
        Arc::new(covenant_audit::InMemoryAuditLog::new()),
    ));
    let key =
        STANDARD.encode([b"\0\0\0\x0bssh-ed25519\0\0\0\x20".as_slice(), &[0x9e; 32]].concat());
    let spec = PhalaEphemeralLeaseSpec {
        schema_version: 1,
        job_id: job,
        operator_pubkey_b58: operator.agent_id().pubkey_base58(),
        lease_generation: format!("phala-ephemeral-{job}-1"),
        lease_expires_at_ms: now + 600_000,
        workload_sha256: "ab".repeat(32),
        relay: PhalaRelayConfig {
            url: endpoint.into(),
            session_id: job.to_string(),
        },
    };
    let envelope = SignedJobEnvelope::sign(
        JobEnvelopePayload {
            job_id: job,
            buyer: buyer.agent_id(),
            kind: JobKind::LeaseSession,
            capability_requirement: CapabilityRequirement {
                inference_tariff: None,
                gpu_class: Some("cpu".into()),
                min_vram_gb: None,
                model_id: Some(PHALA_EPHEMERAL_CPU_MODEL.into()),
                kind: JobKind::LeaseSession,
                max_duration_secs: 600,
                min_reputation_bps: None,
            },
            input: vec![
                covenant_compute_protocol::lease_input(LeaseTerms {
                    max_duration_secs: 600,
                    rate_micro_usdc_per_sec: 1,
                    client_public_key: Some(format!("ssh-ed25519 {key}")),
                })
                .unwrap(),
                phala_ephemeral_lease_input(spec).unwrap(),
            ],
            price_micro_usdc: 600,
            deadline_ms: 900_000,
            idempotency: covenant_a2a::A2AIdempotency::new(
                covenant_a2a::A2ADuplicateSafety::Idempotent,
                "relay-fixture",
            ),
            issued_at_ms: now,
            referral_code: None,
            stream: true,
        },
        &buyer,
    )
    .unwrap();
    let hold =
        EscrowHoldAttestation::sign(job, 600, FundingSource::Organic, now, &operator).unwrap();
    let record = serde_json::from_value(json!({"operator_pubkey_b58":operator.agent_id().pubkey_base58(),"envelope":envelope,"escrow_hold":hold,"phase":"accepted","receipt":null,"output":null,"fee_micro_usdc":0,"accepted_at_ms":now})).unwrap();
    state.jobs().insert(job, record).unwrap();
    let hardware = Arc::new(
        HardwareService::open_signed_leases(state.clone(), &directory.path().join("hardware.json"))
            .unwrap(),
    );
    let relay = RelayService::open(
        state,
        hardware,
        &directory.path().join("relay.json"),
        endpoint.into(),
    )
    .unwrap();
    (directory, relay, buyer, operator, job)
}
fn call(
    service: &RelayService,
    job: Uuid,
    actor: Option<&LocalIdentity>,
    operation: &str,
    arguments: Value,
    token: Option<&str>,
) -> Result<Value> {
    service.dispatch(
        &RelayRequest {
            version: 1,
            session_id: job,
            operation: operation.into(),
            arguments,
        },
        actor.map(|a| a.agent_id().pubkey_base58()).as_deref(),
        token,
        epoch_ms(),
    )
}
fn open(service: &RelayService, job: Uuid, operator: &LocalIdentity) {
    assert_eq!(
        call(
            service,
            job,
            Some(operator),
            "operator_open",
            json!({"app_id":"ab".repeat(20)}),
            None
        )
        .unwrap(),
        json!({"app_id":"ab".repeat(20)})
    );
}
fn registered(service: &RelayService, job: Uuid) -> String {
    call(
        service,
        job,
        None,
        "guest_register",
        json!({"registration_id":Uuid::new_v4(),"instance_id":"cd".repeat(20)}),
        None,
    )
    .unwrap()["guest_token"]
        .as_str()
        .unwrap()
        .into()
}

#[test]
fn allocation_is_assigned_signed_profile_only_and_restart_never_reopens() {
    let (directory, service, buyer, operator, job) = fixture();
    assert_eq!(
        call(
            &service,
            job,
            Some(&buyer),
            "operator_open",
            json!({"app_id":"ab".repeat(20)}),
            None
        )
        .unwrap_err()
        .code,
        "assigned_operator_required"
    );
    open(&service, job, &operator);
    open(&service, job, &operator);
    assert_eq!(
        call(
            &service,
            job,
            Some(&operator),
            "operator_open",
            json!({"app_id":"ff".repeat(20)}),
            None
        )
        .unwrap_err()
        .code,
        "app_replacement_forbidden"
    );
    let state = service.state.clone();
    let hardware = service.hardware.clone();
    let public = service.binding(job).unwrap();
    drop(service);
    let restored = RelayService::open(
        state,
        hardware,
        &directory.path().join("relay.json"),
        "https://relay.example.com".into(),
    )
    .unwrap();
    assert_eq!(restored.binding(job), Some(public));
    assert_eq!(
        call(
            &restored,
            job,
            Some(&operator),
            "operator_open",
            json!({"app_id":"ab".repeat(20)}),
            None
        )
        .unwrap_err()
        .code,
        "session_restart_forbidden"
    );
    assert_eq!(
        restored
            .metadata(job, &buyer.agent_id().pubkey_base58(), epoch_ms())
            .unwrap_err()
            .code,
        "session_not_active"
    );
}

#[test]
fn persistence_failure_never_initializes_a_guest_session_or_recovers_in_place() {
    let (directory, service, _buyer, operator, job) = fixture();
    // Persisting a file over a directory fails deterministically, simulating a
    // failed atomic journal commit without relying on process filesystem mode.
    service.book.lock().path = directory.path().to_path_buf();
    assert_eq!(
        call(
            &service,
            job,
            Some(&operator),
            "operator_open",
            json!({"app_id":"ab".repeat(20)}),
            None
        )
        .unwrap_err()
        .code,
        "relay_unavailable"
    );
    assert!(!service.book.lock().sessions.contains_key(&job));
    assert_eq!(
        call(
            &service,
            job,
            Some(&operator),
            "operator_open",
            json!({"app_id":"ab".repeat(20)}),
            None
        )
        .unwrap_err()
        .code,
        "relay_unavailable"
    );
}

#[test]
fn registration_is_one_candidate_and_pin_is_immutable_durable_public_metadata() {
    let (directory, service, buyer, operator, job) = fixture();
    open(&service, job, &operator);
    let registration = json!({"registration_id":Uuid::new_v4(),"instance_id":"cd".repeat(20)});
    let first = call(
        &service,
        job,
        None,
        "guest_register",
        registration.clone(),
        None,
    )
    .unwrap();
    assert_eq!(
        call(&service, job, None, "guest_register", registration, None).unwrap(),
        first
    );
    assert_eq!(
        registered_replacement(&service, job).unwrap_err().code,
        "guest_replacement_forbidden"
    );
    assert_eq!(
        call(
            &service,
            job,
            None,
            "guest_poll",
            json!({}),
            Some(&"00".repeat(32))
        )
        .unwrap_err()
        .code,
        "unauthorized"
    );
    call(
        &service,
        job,
        Some(&operator),
        "buyer_pin_instance",
        json!({"instance_id":"cd".repeat(20)}),
        None,
    )
    .unwrap();
    let metadata = service
        .metadata(job, &buyer.agent_id().pubkey_base58(), epoch_ms())
        .unwrap();
    assert_eq!(
        metadata.instance_id.as_deref(),
        Some("cd".repeat(20).as_str())
    );
    assert_eq!(
        call(
            &service,
            job,
            Some(&buyer),
            "buyer_pin_instance",
            json!({"instance_id":"ef".repeat(20)}),
            None
        )
        .unwrap_err()
        .code,
        "instance_replacement_forbidden"
    );
    let raw = std::fs::read_to_string(directory.path().join("relay.json")).unwrap();
    assert!(!raw.contains(first["guest_token"].as_str().unwrap()));
    assert!(!raw.contains("registration_id"));
}
fn registered_replacement(service: &RelayService, job: Uuid) -> Result<Value> {
    call(
        service,
        job,
        None,
        "guest_register",
        json!({"registration_id":Uuid::new_v4(),"instance_id":"cd".repeat(20)}),
        None,
    )
}

#[test]
fn mismatched_pin_rejects_candidate_and_cannot_be_adopted_later() {
    let (_directory, service, buyer, operator, job) = fixture();
    open(&service, job, &operator);
    registered(&service, job);
    assert_eq!(
        call(
            &service,
            job,
            Some(&operator),
            "buyer_pin_instance",
            json!({"instance_id":"ef".repeat(20)}),
            None
        )
        .unwrap_err()
        .code,
        "candidate_instance_mismatch"
    );
    assert_eq!(
        call(
            &service,
            job,
            Some(&buyer),
            "buyer_pin_instance",
            json!({"instance_id":"cd".repeat(20)}),
            None
        )
        .unwrap_err()
        .code,
        "session_closed"
    );
}

#[test]
fn no_stream_without_current_verified_hardware_and_buyer_signature() {
    let (_directory, service, buyer, operator, job) = fixture();
    open(&service, job, &operator);
    registered(&service, job);
    call(
        &service,
        job,
        Some(&operator),
        "buyer_pin_instance",
        json!({"instance_id":"cd".repeat(20)}),
        None,
    )
    .unwrap();
    let args = json!({"request_id":Uuid::new_v4(),"kind":"open_stream","payload":{"stream_id":Uuid::new_v4(),"challenge_id":Uuid::new_v4()}});
    assert_eq!(
        call(
            &service,
            job,
            Some(&operator),
            "buyer_submit",
            args.clone(),
            None
        )
        .unwrap_err()
        .code,
        "buyer_required"
    );
    assert_eq!(
        call(&service, job, Some(&buyer), "buyer_submit", args, None)
            .unwrap_err()
            .code,
        "verified_hardware_required"
    );
    assert!(service.book.lock().sessions[&job].operations.is_empty());
}

#[test]
fn controls_preserve_large_identity_idempotency_failure_and_bounded_capacity() {
    let (_directory, service, buyer, operator, job) = fixture();
    open(&service, job, &operator);
    let token = registered(&service, job);
    let id = Uuid::new_v4();
    let args = json!({"request_id":id,"kind":"identity","payload":{}});
    call(
        &service,
        job,
        Some(&operator),
        "buyer_submit",
        args.clone(),
        None,
    )
    .unwrap();
    assert_eq!(
        call(&service, job, None, "guest_poll", json!({}), Some(&token)).unwrap()["operation"],
        args
    );
    let result = json!({"ok":true,"value":{"guest_info":{"tcb_info":"x".repeat(36_445),"app_cert":"y".repeat(14_542)}}});
    let reply = json!({"request_id":id,"result":result});
    call(
        &service,
        job,
        None,
        "guest_reply",
        reply.clone(),
        Some(&token),
    )
    .unwrap();
    call(&service, job, None, "guest_reply", reply, Some(&token)).unwrap();
    assert_eq!(
        call(
            &service,
            job,
            Some(&buyer),
            "buyer_result",
            json!({"request_id":id}),
            None
        )
        .unwrap()["result"],
        result
    );
    call(&service, job, Some(&buyer), "buyer_submit", args, None).unwrap();
    assert!(call(
        &service,
        job,
        None,
        "guest_reply",
        json!({"request_id":id,"result":{"ok":false,"error":"changed"}}),
        Some(&token)
    )
    .is_err());
    for _ in 1..MAX_RELAY_OPERATIONS {
        call(
            &service,
            job,
            Some(&buyer),
            "buyer_submit",
            json!({"request_id":Uuid::new_v4(),"kind":"identity","payload":{}}),
            None,
        )
        .unwrap();
    }
    assert_eq!(
        call(
            &service,
            job,
            Some(&buyer),
            "buyer_submit",
            json!({"request_id":Uuid::new_v4(),"kind":"identity","payload":{}}),
            None
        )
        .unwrap_err()
        .code,
        "operation_budget_exhausted"
    );
}

#[test]
fn close_expiry_and_clock_rollback_stop_every_guest_operation() {
    let (_directory, service, _buyer, operator, job) = fixture();
    open(&service, job, &operator);
    let token = registered(&service, job);
    service
        .state
        .jobs()
        .request_lease_close(job, epoch_ms())
        .unwrap();
    assert_eq!(
        call(&service, job, None, "guest_poll", json!({}), Some(&token))
            .unwrap_err()
            .code,
        "lease_not_live"
    );
    assert!(service.book.lock().sessions[&job].rejected);
    let now = epoch_ms();
    let mut session = Session::new(now, now + 1000);
    assert!(session.live(now + 1000).is_err());
    assert!(session.live(now).is_err());
    let mut session = Session::new(now, now + 1000);
    session.live(now + 1).unwrap();
    assert!(session.live(now).is_err());
}

fn stream_session() -> (Session, Uuid) {
    let now = epoch_ms();
    let mut session = Session::new(now, now + 600_000);
    let instance = "cd".repeat(20);
    session
        .dispatch(
            "guest_register",
            &json!({"registration_id":Uuid::new_v4(),"instance_id":instance}),
            None,
            now,
        )
        .unwrap();
    session.pinned = Some(instance);
    let stream = Uuid::new_v4();
    let request = Uuid::new_v4();
    // Exercise the routing state machine directly. The public service never
    // reaches this path without HardwareService::require_relay_stream.
    session.dispatch("buyer_submit", &json!({"request_id":request,"kind":"open_stream","payload":{"stream_id":stream,"challenge_id":Uuid::new_v4()}}), None, now).unwrap();
    let token = session.guest_token.clone().unwrap();
    session
        .dispatch(
            "guest_reply",
            &json!({"request_id":request,"result":{"ok":true,"value":{"stream_id":stream}}}),
            Some(&token),
            now,
        )
        .unwrap();
    (session, stream)
}
fn exchange(stream: Uuid, offset: usize, data: &[u8], ack: usize) -> Value {
    json!({"stream_id":stream,"operation_id":Uuid::new_v4(),"send_offset":offset,"data":STANDARD.encode(data),"receive_ack":ack,"eof":false})
}

#[test]
fn ciphertext_requires_delivered_ack_and_identical_retries_never_duplicate_bytes() {
    let (mut session, stream) = stream_session();
    let message = exchange(stream, 0, b"encrypted-ssh", 0);
    let first = session.exchange(true, &message).unwrap();
    assert_eq!(session.exchange(true, &message).unwrap(), first);
    assert_eq!(session.stream.as_ref().unwrap().bytes, 13);
    let read = session
        .exchange(false, &exchange(stream, 0, &[], 0))
        .unwrap();
    assert_eq!(
        STANDARD.decode(read["data"].as_str().unwrap()).unwrap(),
        b"encrypted-ssh"
    );
    assert_eq!(session.stream.as_ref().unwrap().buyer.buffer.len(), 13);
    session
        .exchange(false, &exchange(stream, 0, &[], 13))
        .unwrap();
    assert!(session.stream.as_ref().unwrap().buyer.buffer.is_empty());
    let mut changed = message;
    changed["data"] = json!(STANDARD.encode(b"different"));
    assert_eq!(
        session.exchange(true, &changed).unwrap_err().code,
        "exchange_id_collision"
    );
    assert!(session
        .exchange(false, &exchange(stream, 0, &[], 14))
        .is_err());
}

#[test]
fn exchange_retry_cache_eviction_never_reuses_an_old_operation_id() {
    let (mut session, stream) = stream_session();
    let first = exchange(stream, 0, &[], 0);
    session.exchange(true, &first).unwrap();
    for _ in 0..4 {
        session
            .exchange(true, &exchange(stream, 0, &[], 0))
            .unwrap();
    }
    assert_eq!(
        session.exchange(true, &first).unwrap_err().code,
        "exchange_retry_expired"
    );
}

#[test]
fn buffers_combined_byte_limit_one_stream_and_deadline_are_enforced() {
    let (mut session, stream) = stream_session();
    for offset in (0..MAX_RELAY_BUFFER_BYTES).step_by(MAX_RELAY_CHUNK_BYTES) {
        session
            .exchange(
                true,
                &exchange(stream, offset, &vec![1; MAX_RELAY_CHUNK_BYTES], 0),
            )
            .unwrap();
    }
    let full = session
        .exchange(true, &exchange(stream, MAX_RELAY_BUFFER_BYTES, b"one", 0))
        .unwrap();
    assert_eq!(full["send_ack"], MAX_RELAY_BUFFER_BYTES);
    assert_eq!(
        session.stream.as_ref().unwrap().buyer.buffer.len(),
        MAX_RELAY_BUFFER_BYTES
    );
    session.stream.as_mut().unwrap().bytes = MAX_RELAY_STREAM_BYTES;
    assert_eq!(
        session
            .exchange(false, &exchange(stream, 0, b"x", 0))
            .unwrap_err()
            .code,
        "stream_byte_budget_exhausted"
    );
    let (mut session, stream) = stream_session();
    session.stream.as_mut().unwrap().deadline = Instant::now();
    assert_eq!(
        session
            .exchange(true, &exchange(stream, 0, &[], 0))
            .unwrap_err()
            .code,
        "stream_expired"
    );
    assert!(session.dispatch("buyer_submit", &json!({"request_id":Uuid::new_v4(),"kind":"open_stream","payload":{"stream_id":Uuid::new_v4(),"challenge_id":Uuid::new_v4()}}), None, epoch_ms()).is_err());
}

#[test]
fn wire_rejects_duplicate_fields_floats_and_noncanonical_ids() {
    for body in [
        br#"{"x":{"same":1,"same":2}}"#.as_slice(),
        br#"{"value":1.5}"#,
        br#"{"value":NaN}"#,
    ] {
        assert!(serde_json::from_slice::<StrictValue>(body).is_err());
    }
    let job = Uuid::new_v4();
    let valid = json!({"version":1,"session_id":job,"operation":"guest_poll","arguments":{}});
    parse_request(&serde_json::to_vec(&valid).unwrap()).unwrap();
    let mut invalid = valid;
    invalid["session_id"] = json!(job.to_string().to_uppercase());
    assert!(parse_request(&serde_json::to_vec(&invalid).unwrap()).is_err());
}

#[test]
fn actor_signature_binds_exact_body_path_method_time_and_unique_headers() {
    let actor = LocalIdentity::generate("actor@relay");
    let now = epoch_ms();
    let body = b"{\"version\":1}";
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
        binding::sign_request(&actor, "POST", RELAY_PATH, body, now)
            .unwrap()
            .parse()
            .unwrap(),
    );
    authenticate(&headers, "POST", RELAY_PATH, body, now).unwrap();
    assert!(authenticate(&headers, "POST", RELAY_PATH, b"{}", now).is_err());
    assert!(authenticate(&headers, "GET", RELAY_PATH, body, now).is_err());
    assert!(authenticate(&headers, "POST", "/else", body, now).is_err());
    assert!(authenticate(&headers, "POST", RELAY_PATH, body, now + 30_001).is_err());
    headers.append(
        "x-covenant-hardware-signed-at",
        now.to_string().parse().unwrap(),
    );
    assert!(authenticate(&headers, "POST", RELAY_PATH, body, now).is_err());
}

#[test]
fn closing_pending_stream_is_terminal_even_before_guest_open_acknowledgement() {
    let now = epoch_ms();
    let mut session = Session::new(now, now + 600_000);
    let instance = "cd".repeat(20);
    session
        .dispatch(
            "guest_register",
            &json!({"registration_id":Uuid::new_v4(),"instance_id":instance}),
            None,
            now,
        )
        .unwrap();
    session.pinned = Some(instance);
    let request = Uuid::new_v4();
    let stream = Uuid::new_v4();
    session.dispatch("buyer_submit", &json!({"request_id":request,"kind":"open_stream","payload":{"stream_id":stream,"challenge_id":Uuid::new_v4()}}), None, now).unwrap();
    session
        .dispatch("buyer_close_stream", &json!({}), None, now)
        .unwrap();
    session
        .dispatch("buyer_close_stream", &json!({}), None, now)
        .unwrap();
    let token = session.guest_token.clone().unwrap();
    assert_eq!(
        session
            .dispatch(
                "guest_reply",
                &json!({"request_id":request,"result":{"ok":true,"value":{"stream_id":stream}}}),
                Some(&token),
                now
            )
            .unwrap_err()
            .code,
        "stream_closed"
    );
    assert!(session.stream.is_none());
}

#[test]
fn quote_limit_is_enforced_before_guest_quote_work() {
    let now = epoch_ms();
    let mut session = Session::new(now, now + 600_000);
    for _ in 0..8 {
        let id = Uuid::new_v4();
        session
            .dispatch(
                "buyer_submit",
                &json!({"request_id":id,"kind":"quote","payload":{"challenge_id":id}}),
                None,
                now,
            )
            .unwrap();
    }
    let id = Uuid::new_v4();
    assert_eq!(
        session
            .dispatch(
                "buyer_submit",
                &json!({"request_id":id,"kind":"quote","payload":{"challenge_id":id}}),
                None,
                now
            )
            .unwrap_err()
            .code,
        "quote_budget_exhausted"
    );
}

#[tokio::test]
async fn real_http_guest_wire_and_signed_control_interoperate_and_close_fences_poll() {
    let (_directory, service, buyer, operator, job) = fixture();
    let state = service.state.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, router(Arc::new(service)))
            .await
            .unwrap();
    });
    let client = reqwest::Client::new();
    let origin = format!("http://{address}");
    assert_eq!(
        client
            .post(format!("{origin}{RELAY_PATH}?unsigned=query"))
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    let open = RelayRequest {
        version: 1,
        session_id: job,
        operation: "operator_open".into(),
        arguments: json!({"app_id":"ab".repeat(20)}),
    };
    let raw = serde_json::to_vec(&open).unwrap();
    let now = epoch_ms();
    let response = client
        .post(format!("{origin}{RELAY_PATH}"))
        .header("content-type", "application/json")
        .header(
            "x-covenant-hardware-actor",
            operator.agent_id().pubkey_base58(),
        )
        .header("x-covenant-hardware-signed-at", now.to_string())
        .header(
            "x-covenant-hardware-signature",
            binding::sign_request(&operator, "POST", RELAY_PATH, &raw, now).unwrap(),
        )
        .body(raw)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(
        response.json::<RelayResponse>().await.unwrap().result,
        json!({"app_id":"ab".repeat(20)})
    );
    let registration = RelayRequest {
        version: 1,
        session_id: job,
        operation: "guest_register".into(),
        arguments: json!({"registration_id":Uuid::new_v4(),"instance_id":"cd".repeat(20)}),
    };
    let token = client
        .post(format!("{origin}{RELAY_PATH}"))
        .json(&registration)
        .send()
        .await
        .unwrap()
        .json::<RelayResponse>()
        .await
        .unwrap()
        .result["guest_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let path = relay_metadata_path(job);
    let now = epoch_ms();
    let metadata = client
        .get(format!("{origin}{path}"))
        .header(
            "x-covenant-hardware-actor",
            buyer.agent_id().pubkey_base58(),
        )
        .header("x-covenant-hardware-signed-at", now.to_string())
        .header(
            "x-covenant-hardware-signature",
            binding::sign_request(&buyer, "GET", &path, &[], now).unwrap(),
        )
        .send()
        .await
        .unwrap()
        .json::<RelayMetadata>()
        .await
        .unwrap();
    assert_eq!(metadata.app_id, "ab".repeat(20));
    assert!(metadata.instance_id.is_none());
    let poll = RelayRequest {
        version: 1,
        session_id: job,
        operation: "guest_poll".into(),
        arguments: json!({}),
    };
    let reader = {
        let client = client.clone();
        let url = format!("{origin}{RELAY_PATH}");
        tokio::spawn(async move {
            client
                .post(url)
                .bearer_auth(token)
                .json(&poll)
                .send()
                .await
                .unwrap()
        })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    state.jobs().request_lease_close(job, epoch_ms()).unwrap();
    assert_eq!(reader.await.unwrap().status(), StatusCode::GONE);
    task.abort();
}
