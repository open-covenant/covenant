use super::*;

#[test]
fn ephemeral_identity_requires_unique_exact_boot_register_events() {
    let event = |name: &str, byte: u8| TdxEventEntry {
        imr: 3,
        event_type: 0x0800_0001,
        event: name.into(),
        event_payload: hex::encode([byte; 20]),
        digest: "00".repeat(48),
        version: 1,
        preimage: None,
    };
    let valid = vec![event("app-id", 10), event("instance-id", 11)];
    let identity = ephemeral_identity(&valid).unwrap();
    assert_eq!(identity.app_id, "0a".repeat(20));
    assert_eq!(identity.instance_id, "0b".repeat(20));
    assert!(ephemeral_identity(&valid[..1]).is_err());
    let mut duplicate = valid.clone();
    duplicate.push(valid[0].clone());
    assert!(ephemeral_identity(&duplicate).is_err());
    for variant in 0..4 {
        let mut invalid = valid.clone();
        match variant {
            0 => invalid[0].imr = 2,
            1 => invalid[0].event_type = 0x0800_0002,
            2 => invalid[0].event_payload = "ab".repeat(19),
            _ => invalid[0].event_payload = "zz".repeat(20),
        }
        assert!(ephemeral_identity(&invalid).is_err());
    }
}
use p384::pkcs8::DecodePrivateKey;

#[test]
fn collector_tls_golden_binds_transport_and_preserves_legacy_bytes() {
    let challenge: HardwareChallenge =
        serde_json::from_str(include_str!("../reference/phala-collector-challenge.json")).unwrap();
    challenge.validate(1_792_540_800_000).unwrap();
    assert_eq!(
        hex::encode(challenge.guest_report_data().unwrap()),
        "8beb204f70fe63d2648c573860c1a03e1966e9a7084195c4ab4eb0a2d55818a0d1dc8e10aa9a163ab9663320cd4d2f213921d672a904741538b62dfab3c229de"
    );
    let mut legacy = challenge.clone();
    legacy.scope.transport = None;
    assert!(
        !serde_json::to_value(&legacy.scope)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("transport")
    );
    assert_eq!(
        hex::encode(legacy.guest_report_data().unwrap()),
        "a3efeea69d284413d129cced2feec322e2bd247f6e191578ecd364f6e3b5a41c0f3b38f9481291d03ecf97b8e267f62ab917a9db6242a95925d0af4c0d25851c"
    );
}

#[test]
fn changing_outer_transport_invalidates_otherwise_valid_guest_evidence() {
    let mut scope = challenge();
    scope.scope.host = "guest.example".into();
    let mut report = evidence(scope);
    report.challenge.scope.transport = Some(covenant_compute_protocol::ssh::SshTransport::Tls);
    assert!(verify_evidence(&report, report.challenge.issued_at_ms).is_err());
}

fn key() -> SshHostKey {
    let mut bytes = vec![];
    bytes.extend_from_slice(&11u32.to_be_bytes());
    bytes.extend_from_slice(b"ssh-ed25519");
    bytes.extend_from_slice(&32u32.to_be_bytes());
    bytes.extend_from_slice(&[0x9e; 32]);
    SshHostKey {
        algorithm: "ssh-ed25519".into(),
        key_base64: STANDARD.encode(bytes),
    }
}
fn challenge() -> HardwareChallenge {
    let now = 1_780_315_200_000;
    HardwareChallenge::new(
        HardwareScope {
            profile: HardwareProfile::Standard,
            job_id: Uuid::new_v4(),
            operator_pubkey_b58: LocalIdentity::generate("operator@local")
                .agent_id()
                .pubkey_base58(),
            buyer_pubkey_b58: LocalIdentity::generate("buyer@local")
                .agent_id()
                .pubkey_base58(),
            lease_generation: "provider-instance-1".into(),
            host: "127.0.0.1".into(),
            port: 22,
            transport: None,
            username: "worker".into(),
            guest_channel_key: key(),
            workload_sha256: hex::encode([0x3c; 32]),
            lease_expires_at_ms: now + 600_000,
        },
        now,
    )
    .unwrap()
}

#[test]
fn ephemeral_profile_is_report_bound_and_legacy_scope_encoding_is_unchanged() {
    let legacy = challenge();
    let value = serde_json::to_value(&legacy.scope).unwrap();
    assert!(value.get("profile").is_none());
    let mut ephemeral = legacy.clone();
    ephemeral.scope.profile = HardwareProfile::PhalaEphemeralDstack061;
    assert_eq!(
        serde_json::to_value(&ephemeral.scope).unwrap()["profile"],
        "phala_ephemeral_dstack061"
    );
    assert_ne!(
        legacy.guest_report_data().unwrap(),
        ephemeral.guest_report_data().unwrap()
    );
    assert_ne!(
        reference_digest_for_profile(ephemeral.scope.profile),
        reference_digest()
    );
    let mut unsupported = value;
    unsupported["profile"] = serde_json::json!("arbitrary_key_provider");
    assert!(serde_json::from_value::<HardwareScope>(unsupported).is_err());
}

#[test]
fn ephemeral_admission_cannot_be_selected_by_untrusted_evidence_or_used_for_snp_or_gpu() {
    let mut e = evidence(challenge());
    let now = e.challenge.issued_at_ms;
    let ephemeral = HardwareProfile::PhalaEphemeralDstack061;
    assert!(matches!(
        verify_evidence_for_profile(&e, now, ephemeral),
        Err(BindingError::Invalid(_))
    ));
    e.challenge.scope.profile = ephemeral;
    e.challenge.scope.host = "relay.example.com".into();
    e.challenge.scope.port = 443;
    e.challenge.scope.transport = Some(covenant_compute_protocol::ssh::SshTransport::OutboundRelay);
    e.challenge.scope.username = "covenant".into();
    e.challenge.scope.lease_generation = format!("phala-ephemeral-{}-1", e.challenge.scope.job_id);
    assert!(matches!(
        verify_evidence(&e, now),
        Err(BindingError::Invalid(_))
    ));
    assert!(matches!(
        verify_evidence_for_profile(&e, now, ephemeral),
        Err(BindingError::Invalid(_))
    ));
    e.guest = GuestEvidence::Tdx {
        quote_base64: String::new(),
        collateral_json: String::new(),
        events: vec![],
    };
    e.gpu_cc = Some(GpuEvidence {
        report_base64: String::new(),
        certificate_chain_base64: vec![],
    });
    assert!(matches!(
        verify_evidence_for_profile(&e, now, ephemeral),
        Err(BindingError::Invalid(_))
    ));
    // Explicit profile admission must still reach real quote verification.
    e.gpu_cc = None;
    assert!(matches!(
        verify_evidence_for_profile(&e, now, ephemeral),
        Err(BindingError::Verification(_))
    ));
    assert!(verify_evidence_for_profile(&e, e.challenge.expires_at_ms, ephemeral).is_err());
}

#[test]
fn ephemeral_challenge_requires_the_signed_relay_endpoint_and_generation_shape() {
    let mut c = challenge();
    c.scope.profile = HardwareProfile::PhalaEphemeralDstack061;
    c.scope.host = "relay.example.com".into();
    c.scope.port = 443;
    c.scope.transport = Some(covenant_compute_protocol::ssh::SshTransport::OutboundRelay);
    c.scope.username = "covenant".into();
    c.scope.lease_generation = format!("phala-ephemeral-{}-1", c.scope.job_id);
    c.validate(c.issued_at_ms).unwrap();
    let mut cases = vec![];
    let mut changed = c.clone();
    changed.scope.transport = None;
    cases.push(changed);
    let mut changed = c.clone();
    changed.scope.transport = Some(covenant_compute_protocol::ssh::SshTransport::Tls);
    cases.push(changed);
    let mut changed = c.clone();
    changed.scope.host = "127.0.0.1".into();
    cases.push(changed);
    let mut changed = c.clone();
    changed.scope.port = 22;
    cases.push(changed);
    let mut changed = c.clone();
    changed.scope.username = "root".into();
    cases.push(changed);
    let mut changed = c.clone();
    changed.scope.lease_generation = format!("phala-{}-1", c.scope.job_id);
    cases.push(changed);
    for changed in cases {
        assert!(changed.validate(c.issued_at_ms).is_err());
    }
}

#[test]
fn saved_standard_verdict_cannot_be_relabelled_as_ephemeral() {
    let c = challenge();
    let mut e = evidence(c.clone());
    let mut verdict = verify_with_policy(&e, c.issued_at_ms, &Policy::for_tests()).unwrap();
    e.challenge.scope.profile = HardwareProfile::PhalaEphemeralDstack061;
    verdict.challenge.scope.profile = e.challenge.scope.profile;
    assert!(
        verdict
            .require_floor(&e.challenge.scope, TrustClass::Open, c.issued_at_ms)
            .is_err()
    );
    assert!(verify_saved_verdict(&e, &verdict, &e.challenge.scope, c.issued_at_ms).is_err());
    assert!(
        verify_saved_verdict_for_profile(
            &e,
            &verdict,
            &e.challenge.scope,
            c.issued_at_ms,
            HardwareProfile::PhalaEphemeralDstack061
        )
        .is_err()
    );
}
fn evidence(challenge: HardwareChallenge) -> HardwareEvidence {
    let refs: serde_json::Value =
        serde_json::from_str(include_str!("../reference/snp-launch-measurements.json")).unwrap();
    let measurement: [u8; 48] = hex::decode(refs["measurements"][0]["sha384"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let platform: serde_json::Value =
        serde_json::from_str(include_str!("../reference/snp-platform.json")).unwrap();
    let tcb = &platform["tcb_floor"];
    let signing = p384::ecdsa::SigningKey::from_pkcs8_der(include_bytes!(
        "../tests/fixtures/snp/vcek-key.pkcs8.der"
    ))
    .unwrap();
    let report = crate::SnpReportBuilder::genoa(
        challenge.guest_report_data().unwrap(),
        measurement,
        [0x5a; 64],
    )
    .host_data([0x3c; 32])
    .tcb(
        tcb["bootloader"].as_u64().unwrap() as u8,
        tcb["tee"].as_u64().unwrap() as u8,
        tcb["snp"].as_u64().unwrap() as u8,
        tcb["microcode"].as_u64().unwrap() as u8,
    )
    .signed_with(&signing);
    HardwareEvidence {
        challenge,
        guest: GuestEvidence::SevSnp {
            report_base64: STANDARD.encode(report),
            certificate_chain_base64: vec![
                STANDARD.encode(include_bytes!("../tests/fixtures/snp/vcek.der")),
                STANDARD.encode(include_bytes!("../tests/fixtures/snp/ask.der")),
                STANDARD.encode(include_bytes!("../tests/fixtures/snp/test-ark.der")),
            ],
        },
        gpu_cc: None,
    }
}
#[test]
fn saved_verdict_survives_challenge_expiry_but_not_vendor_certificate_expiry() {
    let policy = Policy::for_tests();
    let c = challenge();
    let e = evidence(c.clone());
    let verdict = verify_with_policy(&e, c.issued_at_ms, &policy).unwrap();
    assert!(verify_saved_with_policy(&e, &verdict, &c.scope, c.expires_at_ms + 1, &policy).is_ok());

    // The fixture VCEK expires at 2036-01-01, inside this verdict's TTL.
    // An originally valid signature cannot extend a vendor validity window.
    let expiry = DateTime::parse_from_rfc3339("2036-01-01T00:00:00Z")
        .unwrap()
        .timestamp_millis() as u64;
    let mut scope = c.scope;
    scope.lease_expires_at_ms = expiry + 600_000;
    let c = HardwareChallenge::new(scope, expiry - 60_000).unwrap();
    let e = evidence(c.clone());
    let verdict = verify_with_policy(&e, c.issued_at_ms, &policy).unwrap();
    assert!(matches!(
        verify_saved_with_policy(&e, &verdict, &c.scope, expiry + 1_000, &policy),
        Err(BindingError::Verification(
            VerificationError::SnpCertificateExpired
        ))
    ));
}
#[test]
fn full_uuid_endpoint_key_workload_and_nonce_are_hardware_bound() {
    let c = challenge();
    let e = evidence(c.clone());
    let policy = Policy::for_tests();
    let v = verify_with_policy(&e, c.issued_at_ms, &policy).unwrap();
    assert_eq!(v.evidence_class, TrustClass::Attested);
    assert_eq!(v.served_class, TrustClass::Open);
    assert!(
        v.require_floor(&c.scope, TrustClass::Attested, c.issued_at_ms)
            .is_err()
    );
    let mut mutations = vec![];
    let mut m = c.clone();
    m.scope.job_id = Uuid::new_v4();
    mutations.push(m);
    let mut m = c.clone();
    m.scope.lease_generation.push('x');
    mutations.push(m);
    let mut m = c.clone();
    m.scope.host = "another.example".into();
    mutations.push(m);
    let mut m = c.clone();
    m.nonce_hex = hex::encode([7; 32]);
    mutations.push(m);
    let mut m = c.clone();
    m.scope.guest_channel_key.key_base64 = key().key_base64.replace("np6e", "p6en");
    mutations.push(m);
    let mut m = c.clone();
    m.scope.workload_sha256 = hex::encode([8; 32]);
    mutations.push(m);
    for modified in mutations {
        let mut wrong = e.clone();
        wrong.challenge = modified;
        assert!(verify_with_policy(&wrong, c.issued_at_ms, &policy).is_err());
    }
    assert!(verify_with_policy(&e, c.expires_at_ms, &policy).is_err());
}
#[test]
fn synthetic_roots_never_verify_in_production_or_after_signature_tampering() {
    let c = challenge();
    let mut e = evidence(c.clone());
    assert!(verify_evidence(&e, c.issued_at_ms).is_err());
    if let GuestEvidence::SevSnp { report_base64, .. } = &mut e.guest {
        let mut bytes = STANDARD.decode(&*report_base64).unwrap();
        bytes[200] ^= 1;
        *report_base64 = STANDARD.encode(bytes);
    }
    assert!(verify_with_policy(&e, c.issued_at_ms, &Policy::for_tests()).is_err());
}
#[test]
fn request_signature_binds_method_path_body_and_fresh_time() {
    let identity = LocalIdentity::generate("operator@local");
    let key = identity.agent_id().pubkey_base58();
    let signed = sign_request(&identity, "POST", "/hardware/job", b"evidence", 1000).unwrap();
    assert!(
        verify_request(
            &key,
            &signed,
            "POST",
            "/hardware/job",
            b"evidence",
            1000,
            2000
        )
        .is_ok()
    );
    for (method, path, body) in [
        ("GET", "/hardware/job", b"evidence".as_slice()),
        ("POST", "/hardware/other", b"evidence".as_slice()),
        ("POST", "/hardware/job", b"changed".as_slice()),
    ] {
        assert!(verify_request(&key, &signed, method, path, body, 1000, 2000).is_err());
    }
    assert!(
        verify_request(
            &key,
            &signed,
            "POST",
            "/hardware/job",
            b"evidence",
            1000,
            31001
        )
        .is_err()
    );
}

#[test]
fn a_gpu_report_cannot_be_grafted_onto_an_independent_guest_report() {
    let c = challenge();
    let mut e = evidence(c.clone());
    let gpu = GpuEvidence {
        report_base64: STANDARD.encode([7u8; 16]),
        certificate_chain_base64: vec![STANDARD.encode([8u8; 16])],
    };
    let combined = c.guest_report_data_for_gpu(Some(&gpu)).unwrap();
    assert_ne!(combined, c.guest_report_data().unwrap());
    let mut changed = gpu.clone();
    changed.report_base64 = STANDARD.encode([9u8; 16]);
    assert_ne!(
        combined,
        c.guest_report_data_for_gpu(Some(&changed)).unwrap()
    );
    e.gpu_cc = Some(gpu);
    let error = verify_with_policy(&e, c.issued_at_ms, &Policy::for_tests()).unwrap_err();
    assert!(matches!(
        error,
        BindingError::Verification(VerificationError::SnpReportDataMismatch)
    ));
}
