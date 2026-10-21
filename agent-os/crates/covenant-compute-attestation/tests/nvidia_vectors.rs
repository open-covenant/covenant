//! Vectors for the H100 verifier.
//!
//! The certificates and the positive report under tests/fixtures are generated
//! locally by `regenerate_fixtures` below, because no genuine H100 capture
//! exists yet. Once one is taken from the Dallas box it goes in as
//! tests/fixtures/genuine/{report.bin,chain/*.der} and
//! `genuine_h100_report_verifies` stops being ignored.

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Duration, TimeZone, Utc};
use covenant_compute_attestation::vendor_types::{
    AttestationKind, AttestationVerdict, HostTeeCapability, NodeAttestation, TrustClass,
    attestation_report_nonce,
};
use covenant_compute_attestation::{
    Policy, ReportBuilder, VERIFIER_VERSION, VerificationError, verify_nvidia_gpu_attestation,
};
use p384::ecdsa::SigningKey;
use p384::pkcs8::DecodePrivateKey;
use serde::Deserialize;
use uuid::Uuid;

const BOARD_SERIAL: &str = "1650223000001";
const NODE_ID: &str = "0x2f1c8a9e4d3b7c60a1f5e2d94b8c0176a3e5f9d2";
const DEVICE_PUBLIC_KEY: &str = "kPz9x1Qm0rTf5cGWlYhAeUq2sN7dJoRbXv3iCtZM4uE";
const CHALLENGE_NONCE: [u8; 32] = [
    0x9a, 0x3f, 0x01, 0xd4, 0x77, 0x28, 0xbe, 0x11, 0x5c, 0x0e, 0xa2, 0x64, 0x30, 0x91, 0xff, 0x08,
    0xb7, 0x4c, 0x19, 0x55, 0xe3, 0x6d, 0x82, 0x20, 0x0a, 0xcd, 0x71, 0x3e, 0x46, 0x98, 0x5b, 0xc2,
];

/// Derived the same way the control plane and the node derive it, so the
/// vectors break if that binding ever changes shape.
fn report_nonce(node_id: &str) -> [u8; 32] {
    attestation_report_nonce(&CHALLENGE_NONCE, node_id, DEVICE_PUBLIC_KEY)
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn fixture(name: &str) -> Vec<u8> {
    let path = fixtures().join(name);
    fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "missing fixture {}: {error}. Run: cargo test -p prism-attestation -- --ignored regenerate_fixtures",
            path.display()
        )
    })
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
}

fn device_key() -> SigningKey {
    SigningKey::from_pkcs8_der(&fixture("leaf-key.pkcs8.der")).expect("fixture leaf key")
}

fn chain() -> Vec<String> {
    ["leaf.der", "intermediate.der", "test-root.der"]
        .iter()
        .map(|name| STANDARD.encode(fixture(name)))
        .collect()
}

/// The envelope signature is the control plane's business, not the verifier's,
/// so the vectors leave it empty and exercise the evidence path alone.
fn attestation(evidence: Vec<u8>, chain: Vec<String>) -> NodeAttestation {
    NodeAttestation {
        tdx_event_log: Vec::new(),
        tdx_collateral_json: None,
        node_id: NODE_ID.to_string(),
        challenge_id: Uuid::nil(),
        kind: AttestationKind::NvidiaGpu,
        evidence_base64: STANDARD.encode(evidence),
        certificate_chain_base64: chain,
        capability: HostTeeCapability {
            sev: true,
            sev_es: true,
            sev_snp: false,
            sev_guest_device: true,
            kata_runtime: true,
            kata_confidential_runtime: false,
        },
        pci_address: "0000:01:00.0".to_string(),
        collected_at: now(),
        signature: String::new(),
    }
}

#[derive(Deserialize)]
struct AllowlistFile {
    measurements: Vec<AllowlistEntry>,
}

#[derive(Deserialize)]
struct AllowlistEntry {
    index: u32,
    sha384: Vec<String>,
}

/// The vectors read the same reference file the verifier compiles in, so a
/// change to the allowlist shows up as a failing measurement test rather than
/// as fixtures that quietly stopped covering it.
fn reference_measurements() -> Vec<(u32, [u8; 48])> {
    let raw = include_str!("../reference/h100-measurements.json");
    let file: AllowlistFile = serde_json::from_str(raw).expect("reference measurements");
    file.measurements
        .into_iter()
        .map(|entry| {
            // A slot may list alternatives; a report carries one value, so the
            // vectors build a conforming report from the first.
            let first = entry
                .sha384
                .first()
                .expect("a slot lists at least one digest");
            let digest: [u8; 48] = hex::decode(first)
                .expect("hex digest")
                .try_into()
                .expect("48 byte digest");
            (entry.index, digest)
        })
        .collect()
}

fn good_report_builder() -> ReportBuilder {
    let mut builder = ReportBuilder::h100(report_nonce(NODE_ID), BOARD_SERIAL);
    for (index, digest) in reference_measurements() {
        builder = builder.measurement(index, digest);
    }
    builder
}

/// The vectors chain to the fixture root, which only a test policy anchors.
fn verify(attestation: &NodeAttestation) -> Result<AttestationVerdict, VerificationError> {
    verify_nvidia_gpu_attestation(
        attestation,
        &report_nonce(NODE_ID),
        now(),
        &Policy::for_tests(),
    )
}

#[test]
fn the_vendor_root_is_the_only_anchor_a_service_has() {
    assert_eq!(
        verify_nvidia_gpu_attestation(
            &attestation(fixture("report.bin"), chain()),
            &report_nonce(NODE_ID),
            now(),
            &Policy::default(),
        ),
        Err(VerificationError::UntrustedRoot)
    );
}

#[test]
fn a_valid_report_and_chain_earns_isolated() {
    let verdict = verify(&attestation(fixture("report.bin"), chain())).expect("verdict");

    assert_eq!(verdict.kind, AttestationKind::NvidiaGpu);
    assert_eq!(verdict.node_id, NODE_ID);
    assert_eq!(verdict.granted_class, TrustClass::Isolated);
    assert_eq!(verdict.verifier_version, VERIFIER_VERSION);
    assert_eq!(
        verdict.device_identity,
        format!("prism-test-h100-device/{BOARD_SERIAL}")
    );
    assert_eq!(verdict.measurement_digest.len(), 64);
    assert_eq!(verdict.verified_at, now());
    assert_eq!(verdict.expires_at, now() + Duration::hours(24));
}

#[test]
fn the_verdict_ttl_comes_from_policy() {
    let policy = Policy::for_tests().with_verdict_ttl(Duration::minutes(30));
    let verdict = verify_nvidia_gpu_attestation(
        &attestation(fixture("report.bin"), chain()),
        &report_nonce(NODE_ID),
        now(),
        &policy,
    )
    .expect("verdict");

    assert_eq!(verdict.expires_at, now() + Duration::minutes(30));
}

#[test]
fn a_flipped_byte_in_the_report_breaks_the_report_signature() {
    let mut evidence = fixture("report.bin");
    let last = evidence.len() - 1;
    evidence[last] ^= 0x01;

    assert_eq!(
        verify(&attestation(evidence, chain())),
        Err(VerificationError::ReportSignatureInvalid)
    );
}

#[test]
fn a_flipped_byte_in_the_report_body_breaks_the_report_signature() {
    let mut evidence = fixture("report.bin");
    evidence[45] ^= 0x01;

    assert_eq!(
        verify(&attestation(evidence, chain())),
        Err(VerificationError::ReportSignatureInvalid)
    );
}

#[test]
fn a_flipped_byte_in_the_leaf_certificate_breaks_the_chain() {
    let mut leaf = fixture("leaf.der");
    let last = leaf.len() - 1;
    leaf[last] ^= 0x01;

    let mut chain = chain();
    chain[0] = STANDARD.encode(leaf);

    assert_eq!(
        verify(&attestation(fixture("report.bin"), chain)),
        Err(VerificationError::BrokenChain)
    );
}

#[test]
fn a_chain_rooted_at_an_attacker_root_is_untrusted() {
    let chain: Vec<String> = ["attacker-leaf.der", "attacker-root.der"]
        .iter()
        .map(|name| STANDARD.encode(fixture(name)))
        .collect();

    assert_eq!(
        verify(&attestation(fixture("attacker-report.bin"), chain)),
        Err(VerificationError::UntrustedRoot)
    );
}

/// A report lifted off another machine still has to verify under the leaf key
/// of the chain it arrives with, and it does not.
#[test]
fn a_report_signed_by_another_device_is_rejected() {
    assert_eq!(
        verify(&attestation(fixture("attacker-report.bin"), chain())),
        Err(VerificationError::ReportSignatureInvalid)
    );
}

#[test]
fn a_leaf_on_a_curve_other_than_p384_is_rejected() {
    let chain: Vec<String> = ["p256-leaf.der", "p256-intermediate.der", "test-root.der"]
        .iter()
        .map(|name| STANDARD.encode(fixture(name)))
        .collect();

    assert_eq!(
        verify(&attestation(fixture("report.bin"), chain)),
        Err(VerificationError::WrongSignatureAlgorithm)
    );
}

#[test]
fn a_certificate_authority_may_not_sit_at_the_leaf() {
    let chain: Vec<String> = ["intermediate.der", "test-root.der"]
        .iter()
        .map(|name| STANDARD.encode(fixture(name)))
        .collect();

    assert_eq!(
        verify(&attestation(fixture("report.bin"), chain)),
        Err(VerificationError::MalformedCertificate)
    );
}

#[test]
fn a_certificate_that_is_not_a_certificate_is_rejected() {
    let mut chain = chain();
    chain[0] = STANDARD.encode(b"not a certificate");

    assert_eq!(
        verify(&attestation(fixture("report.bin"), chain)),
        Err(VerificationError::MalformedCertificate)
    );
}

/// The same challenge answered for a different node id hashes to a different
/// nonce, which is what makes a report relayed from another machine useless.
#[test]
fn a_report_bound_to_another_node_grants_nothing() {
    let evidence = good_report_builder()
        .nonce(report_nonce("0x9d41b7e05c2af86310d7e4b95c1f20836ea4d7c1"))
        .signed_with(&device_key());

    assert_eq!(
        verify(&attestation(evidence, chain())),
        Err(VerificationError::NonceMismatch)
    );
}

#[test]
fn a_nonce_one_byte_off_grants_nothing() {
    let mut nonce = report_nonce(NODE_ID);
    nonce[7] ^= 0x01;
    let evidence = good_report_builder()
        .nonce(nonce)
        .signed_with(&device_key());

    assert_eq!(
        verify(&attestation(evidence, chain())),
        Err(VerificationError::NonceMismatch)
    );
}

#[test]
fn an_expired_certificate_is_rejected() {
    let mut chain = chain();
    chain[0] = STANDARD.encode(fixture("expired-leaf.der"));
    let evidence = good_report_builder().signed_with(&expired_device_key());

    assert_eq!(
        verify(&attestation(evidence, chain)),
        Err(VerificationError::CertificateExpired)
    );
}

#[test]
fn the_same_expired_certificate_passes_only_under_the_test_policy() {
    let mut chain = chain();
    chain[0] = STANDARD.encode(fixture("expired-leaf.der"));
    let evidence = good_report_builder().signed_with(&expired_device_key());

    let verdict = verify_nvidia_gpu_attestation(
        &attestation(evidence, chain),
        &report_nonce(NODE_ID),
        now(),
        &Policy::for_tests().allowing_expired_certificates(),
    )
    .expect("verdict");

    assert_eq!(verdict.granted_class, TrustClass::Isolated);
}

#[test]
fn a_measurement_outside_the_reference_set_grants_nothing() {
    let mut builder = ReportBuilder::h100(report_nonce(NODE_ID), BOARD_SERIAL);
    for (index, (name, digest)) in reference_measurements().into_iter().enumerate() {
        let mut digest = digest;
        if index == 0 {
            digest[0] ^= 0x01;
        }
        builder = builder.measurement(name, digest);
    }

    assert_eq!(
        verify(&attestation(builder.signed_with(&device_key()), chain())),
        Err(VerificationError::UnknownMeasurement)
    );
}

#[test]
fn an_unnamed_measurement_grants_nothing() {
    let evidence = good_report_builder()
        .measurement(9_999, [0x11; 48])
        .signed_with(&device_key());

    assert_eq!(
        verify(&attestation(evidence, chain())),
        Err(VerificationError::UnknownMeasurement)
    );
}

#[test]
fn a_report_missing_a_reference_measurement_grants_nothing() {
    let mut builder = ReportBuilder::h100(report_nonce(NODE_ID), BOARD_SERIAL);
    for (name, digest) in reference_measurements().into_iter().skip(1) {
        builder = builder.measurement(name, digest);
    }

    assert_eq!(
        verify(&attestation(builder.signed_with(&device_key()), chain())),
        Err(VerificationError::UnknownMeasurement)
    );
}

#[test]
fn a_device_other_than_an_h100_is_rejected() {
    let evidence = good_report_builder()
        .device_id(0x20b2)
        .signed_with(&device_key());
    assert_eq!(
        verify(&attestation(evidence, chain())),
        Err(VerificationError::UnsupportedDevice)
    );

    let evidence = good_report_builder()
        .vendor_id(0x1002)
        .signed_with(&device_key());
    assert_eq!(
        verify(&attestation(evidence, chain())),
        Err(VerificationError::UnsupportedDevice)
    );
}

#[test]
fn an_empty_chain_is_rejected() {
    assert_eq!(
        verify(&attestation(fixture("report.bin"), Vec::new())),
        Err(VerificationError::MalformedEvidence)
    );
}

#[test]
fn a_five_certificate_chain_is_rejected() {
    let mut chain = chain();
    let leaf = chain[0].clone();
    while chain.len() < 5 {
        chain.insert(0, leaf.clone());
    }

    assert_eq!(
        verify(&attestation(fixture("report.bin"), chain)),
        Err(VerificationError::ChainTooLong)
    );
}

#[test]
fn evidence_of_another_kind_is_rejected() {
    for kind in [
        AttestationKind::SevSnp,
        AttestationKind::Tdx,
        AttestationKind::NvidiaCc,
    ] {
        let mut attestation = attestation(fixture("report.bin"), chain());
        attestation.kind = kind;

        assert_eq!(
            verify(&attestation),
            Err(VerificationError::MalformedEvidence)
        );
    }
}

#[test]
fn no_report_can_earn_more_than_isolated() {
    for cc_mode in [0u8, 1, 2] {
        let evidence = good_report_builder()
            .cc_mode(cc_mode)
            .signed_with(&device_key());
        let verdict = verify(&attestation(evidence, chain())).expect("verdict");
        assert_eq!(verdict.granted_class, TrustClass::Isolated);
    }
}

/// Confidential mode is carried into the digest, so the same measurements on a
/// GPU in a different state do not hash to the same commitment.
#[test]
fn the_measurement_digest_covers_the_cc_mode() {
    let off = verify(&attestation(
        good_report_builder().cc_mode(0).signed_with(&device_key()),
        chain(),
    ))
    .expect("verdict");
    let on = verify(&attestation(
        good_report_builder().cc_mode(1).signed_with(&device_key()),
        chain(),
    ))
    .expect("verdict");

    assert_ne!(off.measurement_digest, on.measurement_digest);
}

#[test]
#[ignore = "waiting on a capture from the Dallas H100"]
fn genuine_h100_report_verifies() {
    let report = fs::read(fixtures().join("genuine/report.bin")).expect("genuine report");
    let chain: Vec<String> = ["leaf.der", "intermediate.der", "root.der"]
        .iter()
        .map(|name| STANDARD.encode(fs::read(fixtures().join("genuine/chain").join(name)).unwrap()))
        .collect();

    let verdict = verify(&attestation(report, chain)).expect("verdict");
    assert_eq!(verdict.granted_class, TrustClass::Isolated);
}

/// Regenerates every locally generated fixture. Ignored because it rewrites
/// checked-in files and produces fresh keys each run.
#[test]
#[ignore = "rewrites checked-in fixtures"]
fn regenerate_fixtures() {
    use rcgen::{
        BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose,
        PKCS_ECDSA_P384_SHA384, date_time_ymd,
    };

    fn params(common_name: &str, ca: IsCa) -> CertificateParams {
        let mut params = CertificateParams::default();
        params
            .distinguished_name
            .push(DnType::CommonName, common_name);
        if !matches!(ca, IsCa::ExplicitNoCa) {
            params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        }
        params.is_ca = ca;
        params.not_before = date_time_ymd(2026, 1, 1);
        params.not_after = date_time_ymd(2036, 1, 1);
        params
    }

    fn signing_key(key_pair: &KeyPair) -> SigningKey {
        SigningKey::from_pkcs8_der(&key_pair.serialize_der()).expect("pkcs8 round trip")
    }

    let dir = fixtures();
    fs::create_dir_all(&dir).expect("fixture directory");

    let root_key = KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).expect("root key");
    let root_params = params(
        "prism-test attestation root",
        IsCa::Ca(BasicConstraints::Unconstrained),
    );
    let root = root_params
        .self_signed(&root_key)
        .expect("root certificate");
    let root_issuer = Issuer::from_params(&root_params, &root_key);

    let intermediate_key =
        KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).expect("intermediate key");
    let intermediate_params = params(
        "prism-test device identity CA",
        IsCa::Ca(BasicConstraints::Constrained(0)),
    );
    let intermediate = intermediate_params
        .signed_by(&intermediate_key, &root_issuer)
        .expect("intermediate certificate");
    let intermediate_issuer = Issuer::from_params(&intermediate_params, &intermediate_key);

    let leaf_key = KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).expect("leaf key");
    let leaf = params("prism-test-h100-device", IsCa::ExplicitNoCa)
        .signed_by(&leaf_key, &intermediate_issuer)
        .expect("leaf certificate");

    let expired_leaf_key =
        KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).expect("expired leaf key");
    let mut expired_params = params("prism-test-h100-device", IsCa::ExplicitNoCa);
    expired_params.not_before = date_time_ymd(2026, 1, 1);
    expired_params.not_after = date_time_ymd(2026, 3, 1);
    let expired_leaf = expired_params
        .signed_by(&expired_leaf_key, &intermediate_issuer)
        .expect("expired leaf certificate");

    let p256_intermediate_key =
        KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("p256 intermediate key");
    let p256_intermediate_params = params(
        "prism-test p256 device identity CA",
        IsCa::Ca(BasicConstraints::Constrained(0)),
    );
    let p256_intermediate = p256_intermediate_params
        .signed_by(&p256_intermediate_key, &root_issuer)
        .expect("p256 intermediate certificate");
    let p256_leaf = params("prism-test-h100-device", IsCa::ExplicitNoCa)
        .signed_by(
            &KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("p256 leaf key"),
            &Issuer::from_params(&p256_intermediate_params, &p256_intermediate_key),
        )
        .expect("p256 leaf certificate");

    let attacker_key = KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).expect("attacker root key");
    let attacker_params = params(
        "prism-test attestation root",
        IsCa::Ca(BasicConstraints::Unconstrained),
    );
    let attacker_root = attacker_params
        .self_signed(&attacker_key)
        .expect("attacker root certificate");
    let attacker_issuer = Issuer::from_params(&attacker_params, &attacker_key);
    let attacker_leaf_key =
        KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).expect("attacker leaf key");
    let attacker_leaf = params("prism-test-h100-device", IsCa::ExplicitNoCa)
        .signed_by(&attacker_leaf_key, &attacker_issuer)
        .expect("attacker leaf certificate");

    let write = |name: &str, bytes: &[u8]| {
        fs::write(dir.join(name), bytes).unwrap_or_else(|error| panic!("write {name}: {error}"))
    };

    write("test-root.der", root.der());
    write("intermediate.der", intermediate.der());
    write("leaf.der", leaf.der());
    write("leaf-key.pkcs8.der", &leaf_key.serialize_der());
    write("expired-leaf.der", expired_leaf.der());
    write(
        "expired-leaf-key.pkcs8.der",
        &expired_leaf_key.serialize_der(),
    );
    write("p256-intermediate.der", p256_intermediate.der());
    write("p256-leaf.der", p256_leaf.der());
    write("attacker-root.der", attacker_root.der());
    write("attacker-leaf.der", attacker_leaf.der());
    write(
        "report.bin",
        &good_report_builder().signed_with(&signing_key(&leaf_key)),
    );
    write(
        "attacker-report.bin",
        &good_report_builder().signed_with(&signing_key(&attacker_leaf_key)),
    );
}

fn expired_device_key() -> SigningKey {
    SigningKey::from_pkcs8_der(&fixture("expired-leaf-key.pkcs8.der")).expect("expired leaf key")
}
