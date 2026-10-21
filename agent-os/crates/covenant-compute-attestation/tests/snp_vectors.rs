//! Vectors for the SEV-SNP guest verifier.
//!
//! The certificates and the positive report under tests/fixtures/snp are
//! generated locally by `regenerate_fixtures` below, because no genuine Genoa
//! capture exists yet. Once one is taken from the Dallas box it goes in as
//! tests/fixtures/snp/genuine/{report.bin,chain/*.der} and
//! `genuine_genoa_report_verifies` stops being ignored.

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Duration, TimeZone, Utc};
use covenant_compute_attestation::vendor_types::{
    AttestationKind, GuestAttestation, LeaseAttestationVerdict, MAX_VERIFIABLE_TRUST_CLASS, SnpTcb,
    TrustClass, class_for_lease, snp_report_data,
};
use covenant_compute_attestation::{
    Policy, SNP_VERIFIER_VERSION, SnpExpectation, SnpReportBuilder, VerificationError,
    verify_sev_snp_attestation,
};
use p384::ecdsa::SigningKey;
use p384::pkcs8::DecodePrivateKey;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const NODE_ID: &str = "0x2f1c8a9e4d3b7c60a1f5e2d94b8c0176a3e5f9d2";
const LEASE_ID: u64 = 4711;
const CHALLENGE_NONCE: [u8; 32] = [
    0x71, 0x0c, 0xd8, 0x35, 0x9f, 0x42, 0xa6, 0x1b, 0xe0, 0x57, 0x2d, 0x94, 0x38, 0xc1, 0x6a, 0xff,
    0x0b, 0x83, 0x51, 0xd7, 0x2e, 0x49, 0xba, 0x16, 0xc5, 0x30, 0x78, 0xe2, 0x9d, 0x44, 0x1f, 0x60,
];
const CHIP_ID: [u8; 64] = [0x5a; 64];
const HOST_DATA: [u8; 32] = [0x3c; 32];

/// The floor reference/snp-platform.json pins for this product line. Read from
/// the file rather than repeated here: a copy drifts the moment the real floor
/// moves, and every fixture below is built relative to it.
#[derive(serde::Deserialize)]
struct PlatformFile {
    tcb_floor: TcbFloorEntry,
}

#[derive(serde::Deserialize)]
struct TcbFloorEntry {
    bootloader: u8,
    tee: u8,
    snp: u8,
    microcode: u8,
}

fn tcb_floor() -> SnpTcb {
    let raw = include_str!("../reference/snp-platform.json");
    let file: PlatformFile = serde_json::from_str(raw).expect("reference platform");
    SnpTcb {
        bootloader: file.tcb_floor.bootloader,
        tee: file.tcb_floor.tee,
        snp: file.tcb_floor.snp,
        microcode: file.tcb_floor.microcode,
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/snp")
}

fn fixture(name: &str) -> Vec<u8> {
    let path = fixtures().join(name);
    fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "missing fixture {}: {error}. Run: cargo test --release -p prism-attestation -- --ignored regenerate_fixtures",
            path.display()
        )
    })
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
}

fn vcek_key() -> SigningKey {
    SigningKey::from_pkcs8_der(&fixture("vcek-key.pkcs8.der")).expect("fixture VCEK key")
}

fn chain() -> Vec<String> {
    chain_of(&["vcek.der", "ask.der", "test-ark.der"])
}

fn chain_of(names: &[&str]) -> Vec<String> {
    names
        .iter()
        .map(|name| STANDARD.encode(fixture(name)))
        .collect()
}

/// The OpenSSH line the guest generates for this lease. Only the base64 blob
/// matters to the verifier; the fingerprint it publishes is taken from it.
fn channel_key() -> String {
    let mut blob = Vec::with_capacity(51);
    blob.extend_from_slice(&11_u32.to_be_bytes());
    blob.extend_from_slice(b"ssh-ed25519");
    blob.extend_from_slice(&32_u32.to_be_bytes());
    blob.extend_from_slice(&[0x9e; 32]);
    format!("ssh-ed25519 {} prism-lease-4711", STANDARD.encode(&blob))
}

fn report_data() -> [u8; 64] {
    snp_report_data(&CHALLENGE_NONCE, LEASE_ID, &channel_key())
}

fn expectation() -> SnpExpectation {
    SnpExpectation {
        report_data: report_data(),
        host_data: HOST_DATA,
        chip_id_digest: Some(Sha256::digest(CHIP_ID).into()),
    }
}

/// The envelope signature is the control plane's business, not the verifier's,
/// so the vectors leave it empty and exercise the evidence path alone.
fn attestation(report: Vec<u8>, chain: Vec<String>) -> GuestAttestation {
    GuestAttestation {
        node_id: NODE_ID.to_string(),
        lease_id: LEASE_ID,
        challenge_id: Uuid::nil(),
        kind: AttestationKind::SevSnp,
        report_base64: STANDARD.encode(report),
        certificate_chain_base64: chain,
        guest_channel_key: channel_key(),
        collected_at: now(),
        signature: String::new(),
    }
}

#[derive(Deserialize)]
struct LaunchMeasurementFile {
    measurements: Vec<LaunchMeasurementEntry>,
}

#[derive(Deserialize)]
struct LaunchMeasurementEntry {
    sha384: String,
}

/// The vectors read the same reference file the verifier compiles in, so a
/// change to the launch measurements shows up as a failing measurement test
/// rather than as fixtures that quietly stopped covering it.
fn reference_measurement() -> [u8; 48] {
    let raw = include_str!("../reference/snp-launch-measurements.json");
    let file: LaunchMeasurementFile = serde_json::from_str(raw).expect("reference measurements");
    hex::decode(&file.measurements.first().expect("one measurement").sha384)
        .expect("hex digest")
        .try_into()
        .expect("48 byte digest")
}

fn good_report_builder() -> SnpReportBuilder {
    SnpReportBuilder::genoa(report_data(), reference_measurement(), CHIP_ID)
        .host_data(HOST_DATA)
        .tcb(
            tcb_floor().bootloader,
            tcb_floor().tee,
            tcb_floor().snp,
            tcb_floor().microcode,
        )
}

/// The vectors chain to the fixture ARK, which only a test policy anchors.
fn verify(attestation: &GuestAttestation) -> Result<LeaseAttestationVerdict, VerificationError> {
    verify_sev_snp_attestation(attestation, &expectation(), now(), &Policy::for_tests())
}

#[test]
fn the_amd_root_is_the_only_anchor_a_service_has() {
    assert_eq!(
        verify_sev_snp_attestation(
            &attestation(fixture("report.bin"), chain()),
            &expectation(),
            now(),
            &Policy::default(),
        ),
        Err(VerificationError::SnpUntrustedRoot)
    );
}

#[test]
fn a_good_report_and_chain_earn_attested_while_the_served_class_stays_clamped() {
    let verdict = verify(&attestation(fixture("report.bin"), chain())).expect("verdict");

    assert_eq!(verdict.kind, AttestationKind::SevSnp);
    assert_eq!(verdict.lease_id, LEASE_ID);
    assert_eq!(verdict.node_id, NODE_ID);
    assert_eq!(verdict.granted_class, TrustClass::Attested);
    assert_eq!(verdict.verifier_version, SNP_VERIFIER_VERSION);
    assert_eq!(
        verdict.guest.measurement,
        hex::encode(reference_measurement())
    );
    assert_eq!(verdict.guest.host_data, hex::encode(HOST_DATA));
    assert_eq!(
        verdict.guest.image_digest,
        format!("sha256:{}", hex::encode(HOST_DATA))
    );
    assert_eq!(
        verdict.guest.chip_id_digest,
        hex::encode(Sha256::digest(CHIP_ID))
    );
    assert_eq!(verdict.guest.reported_tcb, tcb_floor());
    assert!(!verdict.guest.policy_debug);
    assert_eq!(verdict.guest.vmpl, 0);
    assert!(verdict.guest.channel_key_fingerprint.starts_with("SHA256:"));
    assert_eq!(verdict.verified_at, now());
    assert_eq!(verdict.expires_at, now() + Duration::days(7));

    // The verifier says what the evidence earns; the protocol decides what the
    // network serves and clamps it to what the checked-in material can back.
    // Those agree for SNP today, so the clamp changes nothing here. It is still
    // the thing that stops a future verdict outrunning the evidence.
    assert_eq!(
        class_for_lease(
            LEASE_ID,
            NODE_ID,
            TrustClass::Isolated,
            Some(&verdict),
            None,
            None,
            now()
        ),
        TrustClass::Attested
    );
    assert!(TrustClass::Attested <= MAX_VERIFIABLE_TRUST_CLASS);
}

#[test]
fn the_lease_verdict_ttl_comes_from_policy() {
    let verdict = verify_sev_snp_attestation(
        &attestation(fixture("report.bin"), chain()),
        &expectation(),
        now(),
        &Policy::for_tests().with_lease_verdict_ttl(Duration::hours(6)),
    )
    .expect("verdict");

    assert_eq!(verdict.expires_at, now() + Duration::hours(6));
}

/// Every byte from 0x000 to 0x29F is signed, so a change anywhere in that range
/// has to break the signature rather than only the fields a check reads.
#[test]
fn a_flipped_byte_anywhere_in_the_signed_region_breaks_the_signature() {
    for offset in [0x004, 0x048, 0x14c, 0x188, 0x1e8, 0x29f] {
        let mut report = fixture("report.bin");
        report[offset] ^= 0x01;

        assert_eq!(
            verify(&attestation(report, chain())),
            Err(VerificationError::SnpReportSignatureInvalid),
            "byte {offset:#x} is not covered by the report signature"
        );
    }
}

#[test]
fn a_vcek_issued_to_another_chip_is_rejected() {
    assert_eq!(
        verify(&attestation(
            fixture("report.bin"),
            chain_of(&["vcek-wrong-hwid.der", "ask.der", "test-ark.der"])
        )),
        Err(VerificationError::SnpVcekChipMismatch)
    );
}

#[test]
fn a_vcek_below_the_reported_tcb_is_rejected() {
    assert_eq!(
        verify(&attestation(
            fixture("report.bin"),
            chain_of(&["vcek-low-svn.der", "ask.der", "test-ark.der"])
        )),
        Err(VerificationError::SnpVcekTcbMismatch)
    );
}

#[test]
fn a_chain_rooted_at_an_attacker_ark_is_untrusted() {
    let report = good_report_builder().signed_with(&attacker_vcek_key());

    assert_eq!(
        verify(&attestation(
            report,
            chain_of(&["attacker-vcek.der", "attacker-ask.der", "attacker-ark.der"])
        )),
        Err(VerificationError::SnpUntrustedRoot)
    );
}

#[test]
fn an_ask_signed_with_pkcs1_v15_is_rejected() {
    assert_eq!(
        verify(&attestation(
            fixture("report.bin"),
            chain_of(&["vcek.der", "ask-pkcs1v15.der", "test-ark.der"])
        )),
        Err(VerificationError::SnpChainAlgorithm)
    );
}

#[test]
fn a_chain_that_is_not_three_certificates_is_rejected() {
    for names in [
        vec!["vcek.der", "test-ark.der"],
        vec!["vcek.der", "ask.der", "ask.der", "test-ark.der"],
        vec![],
    ] {
        assert_eq!(
            verify(&attestation(fixture("report.bin"), chain_of(&names))),
            Err(VerificationError::SnpChainShape)
        );
    }
}

#[test]
fn report_data_one_byte_off_grants_nothing() {
    let mut report_data = report_data();
    report_data[13] ^= 0x01;
    let report = good_report_builder()
        .report_data(report_data)
        .signed_with(&vcek_key());

    assert_eq!(
        verify(&attestation(report, chain())),
        Err(VerificationError::SnpReportDataMismatch)
    );
}

/// A report taken for another lease commits to a different report data, which
/// is what stops one guest's evidence being presented for another's session.
#[test]
fn a_report_bound_to_another_lease_grants_nothing() {
    let report = good_report_builder()
        .report_data(snp_report_data(
            &CHALLENGE_NONCE,
            LEASE_ID + 1,
            &channel_key(),
        ))
        .signed_with(&vcek_key());

    assert_eq!(
        verify(&attestation(report, chain())),
        Err(VerificationError::SnpReportDataMismatch)
    );
}

#[test]
fn host_data_one_byte_off_grants_nothing() {
    let mut host_data = HOST_DATA;
    host_data[0] ^= 0x01;
    let report = good_report_builder()
        .host_data(host_data)
        .signed_with(&vcek_key());

    assert_eq!(
        verify(&attestation(report, chain())),
        Err(VerificationError::SnpHostDataMismatch)
    );
}

#[test]
fn the_debug_policy_bit_grants_nothing() {
    let report = good_report_builder()
        .policy_bit(19)
        .signed_with(&vcek_key());

    assert_eq!(
        verify(&attestation(report, chain())),
        Err(VerificationError::SnpDebugPolicyEnabled)
    );
}

#[test]
fn a_migration_agent_grants_nothing() {
    let report = good_report_builder()
        .policy_bit(18)
        .signed_with(&vcek_key());

    assert_eq!(
        verify(&attestation(report, chain())),
        Err(VerificationError::SnpMigrationAgentAllowed)
    );
}

#[test]
fn a_launch_policy_other_than_the_reference_one_grants_nothing() {
    let report = good_report_builder()
        .policy_bit(20)
        .signed_with(&vcek_key());

    assert_eq!(
        verify(&attestation(report, chain())),
        Err(VerificationError::SnpGuestPolicyMismatch)
    );
}

#[test]
fn a_report_taken_at_another_vmpl_grants_nothing() {
    let report = good_report_builder().vmpl(1).signed_with(&vcek_key());

    assert_eq!(
        verify(&attestation(report, chain())),
        Err(VerificationError::SnpWrongVmpl)
    );
}

#[test]
fn a_measurement_outside_the_reference_set_grants_nothing() {
    let mut measurement = reference_measurement();
    measurement[47] ^= 0x01;
    let report = good_report_builder()
        .measurement(measurement)
        .signed_with(&vcek_key());

    assert_eq!(
        verify(&attestation(report, chain())),
        Err(VerificationError::SnpUnknownLaunchMeasurement)
    );
}

/// The VCEK's own SVNs move with the report's reported TCB, so lowering both
/// keeps the chain consistent and leaves the floor as the check that fires.
#[test]
fn a_tcb_one_below_the_floor_grants_nothing() {
    let report = good_report_builder()
        .tcb(
            tcb_floor().bootloader,
            tcb_floor().tee,
            tcb_floor().snp,
            tcb_floor().microcode - 1,
        )
        .signed_with(&vcek_key());

    assert_eq!(
        verify(&attestation(
            report,
            chain_of(&["vcek-low-svn.der", "ask.der", "test-ark.der"])
        )),
        Err(VerificationError::SnpTcbBelowFloor)
    );
}

/// Reported TCB is what the VCEK was keyed against and says nothing about the
/// platform the VM actually launched on.
#[test]
fn a_platform_downgraded_before_launch_grants_nothing() {
    let report = good_report_builder()
        .launch_tcb(
            tcb_floor().bootloader,
            tcb_floor().tee,
            tcb_floor().snp - 1,
            tcb_floor().microcode,
        )
        .signed_with(&vcek_key());

    assert_eq!(
        verify(&attestation(report, chain())),
        Err(VerificationError::SnpTcbBelowFloor)
    );
}

#[test]
fn a_report_of_any_other_length_is_rejected() {
    let good = fixture("report.bin");
    for report in [good[..good.len() - 1].to_vec(), [good, vec![0]].concat()] {
        assert_eq!(
            verify(&attestation(report, chain())),
            Err(VerificationError::SnpReportWrongSize)
        );
    }
}

#[test]
fn a_signature_field_wider_than_a_scalar_is_rejected_rather_than_truncated() {
    let report = good_report_builder().signed_with_oversized_r(&vcek_key());

    assert_eq!(
        verify(&attestation(report, chain())),
        Err(VerificationError::SnpSignatureFieldOutOfRange)
    );
}

#[test]
fn a_report_of_an_unsupported_abi_version_is_rejected() {
    // 2 and 3 are both real ABI revisions and both accepted. 4 is not one.
    let report = good_report_builder().version(4).signed_with(&vcek_key());

    assert_eq!(
        verify(&attestation(report, chain())),
        Err(VerificationError::SnpReportVersionUnsupported)
    );
}

#[test]
fn a_report_from_a_chip_this_node_is_not_bound_to_grants_nothing() {
    let mut expectation = expectation();
    expectation.chip_id_digest = Some(Sha256::digest([0x11_u8; 64]).into());

    assert_eq!(
        verify_sev_snp_attestation(
            &attestation(fixture("report.bin"), chain()),
            &expectation,
            now(),
            &Policy::for_tests(),
        ),
        Err(VerificationError::SnpChipMismatch)
    );
}

#[test]
fn evidence_of_another_kind_is_rejected() {
    for kind in [
        AttestationKind::Tdx,
        AttestationKind::NvidiaCc,
        AttestationKind::NvidiaGpu,
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
fn a_guest_channel_key_that_is_not_an_openssh_line_is_rejected() {
    let mut attestation = attestation(fixture("report.bin"), chain());
    attestation.guest_channel_key = "ssh-ed25519".to_string();

    assert_eq!(
        verify(&attestation),
        Err(VerificationError::SnpChannelKeyMalformed)
    );
}

#[derive(Deserialize)]
struct ProvenanceFile {
    artifacts: Vec<ProvenanceEntry>,
}

#[derive(Deserialize)]
struct ProvenanceEntry {
    path: String,
    rung: String,
    state: String,
}

/// The ceiling is tied to the material on disk rather than to memory. While any
/// artifact behind a rung is a placeholder, no node and no lease can earn that
/// rung from real evidence, and claiming it would be asserting a property the
/// network cannot substantiate.
#[test]
fn the_ceiling_matches_the_evidence_on_file() {
    let raw = include_str!("../reference/provenance.json");
    let file: ProvenanceFile = serde_json::from_str(raw).expect("reference/provenance.json");

    for required in [
        "roots/amd-ark-genoa.der",
        "reference/snp-platform.json",
        "reference/snp-launch-measurements.json",
    ] {
        assert!(
            file.artifacts.iter().any(|entry| entry.path == required),
            "{required} is not recorded in reference/provenance.json"
        );
    }

    for entry in &file.artifacts {
        // `vendor-published` is a root fetched from the vendor that signs it,
        // which is a different thing from `captured`, a measurement read off
        // our own hardware. Both are real enough to anchor a rung; conflating
        // them would let a downloaded certificate pass as evidence about this
        // machine. `empty-refuses-all` is a reference set with no entries: it
        // can anchor nothing and refuses everything, which is safe at any
        // ceiling exactly as long as it really is empty.
        assert!(
            matches!(
                entry.state.as_str(),
                "placeholder" | "captured" | "vendor-published" | "empty-refuses-all"
            ),
            "{} has an unrecognised provenance state {}",
            entry.path,
            entry.state
        );
        if entry.state == "empty-refuses-all" {
            let raw = std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(&entry.path),
            )
            .unwrap_or_else(|_| panic!("{} is recorded but not on disk", entry.path));
            let value: serde_json::Value = serde_json::from_str(&raw).expect("reference JSON");
            let entries = value
                .get("launch_measurements")
                .and_then(|list| list.as_array())
                .unwrap_or_else(|| {
                    panic!("{} does not carry a launch_measurements list", entry.path)
                });
            assert!(
                entries.is_empty(),
                "{} says empty-refuses-all but carries {} entries; record what they are",
                entry.path,
                entries.len()
            );
            continue;
        }
        if entry.state != "placeholder" {
            continue;
        }
        let rung = match entry.rung.as_str() {
            "isolated" => TrustClass::Isolated,
            "attested" => TrustClass::Attested,
            "confidential" => TrustClass::Confidential,
            other => panic!("{} names an unrecognised rung {other}", entry.path),
        };
        // Isolated is served on a posture and a tunnel the control plane checks
        // itself, and GPU evidence tightens that rather than creating it, so its
        // placeholders are recorded here rather than asserted on. Everything
        // above it exists only because of the material in this crate.
        if rung <= TrustClass::Isolated {
            continue;
        }
        assert!(
            MAX_VERIFIABLE_TRUST_CLASS < rung,
            "{} is still a placeholder, so the ceiling may not stand at {}",
            entry.path,
            MAX_VERIFIABLE_TRUST_CLASS.label()
        );
    }
}

#[test]
fn genuine_genoa_report_verifies() {
    let report = fs::read(fixtures().join("genuine/report.bin")).expect("genuine report");
    let chain: Vec<String> = ["vcek.der", "ask.der", "ark.der"]
        .iter()
        .map(|name| STANDARD.encode(fs::read(fixtures().join("genuine/chain").join(name)).unwrap()))
        .collect();

    // Taken from prism-node-01 on 2026-08-17 with a random nonce, outside any
    // lease, so it carries none of this crate's lease binding. What it does
    // prove is every platform check ahead of that binding: a real VERSION 3
    // report, signed by a VCEK AMD issued for that chip, chaining to the real
    // ARK, at VMPL0, whose launch measurement equals the one sev-snp-measure
    // computes from the recorded boot inputs, on a platform at or above the TCB
    // floor. Reaching the report-data check is therefore the pass condition;
    // asserting Attested here would require inventing a lease this capture
    // never belonged to.
    let captured_at = Utc.with_ymd_and_hms(2026, 8, 17, 19, 15, 0).unwrap();

    assert_eq!(
        verify_sev_snp_attestation(
            &attestation(report, chain),
            &expectation(),
            captured_at,
            &Policy::default(),
        ),
        Err(VerificationError::SnpReportDataMismatch)
    );
}

fn attacker_vcek_key() -> SigningKey {
    SigningKey::from_pkcs8_der(&fixture("attacker-vcek-key.pkcs8.der")).expect("attacker VCEK key")
}

/// A DER writer small enough to read in one sitting. rcgen cannot sign with
/// RSASSA-PSS over SHA-384 and cannot place the AMD extensions, and building the
/// certificates here is also what lets a negative vector differ from the
/// positive one in exactly one field.
mod der {
    pub fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        if body.len() < 0x80 {
            out.push(body.len() as u8);
        } else {
            let length = body.len().to_be_bytes();
            let significant = &length[length.iter().position(|byte| *byte != 0).unwrap()..];
            out.push(0x80 | significant.len() as u8);
            out.extend_from_slice(significant);
        }
        out.extend_from_slice(body);
        out
    }

    pub fn seq(parts: Vec<Vec<u8>>) -> Vec<u8> {
        tlv(0x30, &parts.concat())
    }

    pub fn oid(dotted: &str) -> Vec<u8> {
        let arcs: Vec<u64> = dotted
            .split('.')
            .map(|arc| arc.parse().expect("numeric arc"))
            .collect();
        let mut body = vec![(arcs[0] * 40 + arcs[1]) as u8];
        for arc in &arcs[2..] {
            let mut chunk = vec![(arc & 0x7f) as u8];
            let mut rest = arc >> 7;
            while rest > 0 {
                chunk.insert(0, 0x80 | (rest & 0x7f) as u8);
                rest >>= 7;
            }
            body.extend_from_slice(&chunk);
        }
        tlv(0x06, &body)
    }

    pub fn integer(value: u64) -> Vec<u8> {
        let bytes = value.to_be_bytes();
        let start = bytes
            .iter()
            .position(|byte| *byte != 0)
            .unwrap_or(bytes.len() - 1);
        let mut body = bytes[start..].to_vec();
        if body[0] & 0x80 != 0 {
            body.insert(0, 0);
        }
        tlv(0x02, &body)
    }

    pub fn octet_string(body: &[u8]) -> Vec<u8> {
        tlv(0x04, body)
    }

    pub fn bit_string(body: &[u8]) -> Vec<u8> {
        tlv(0x03, &[&[0_u8][..], body].concat())
    }

    pub fn boolean(value: bool) -> Vec<u8> {
        vec![0x01, 0x01, if value { 0xff } else { 0x00 }]
    }

    pub fn utf8(value: &str) -> Vec<u8> {
        tlv(0x0c, value.as_bytes())
    }

    pub fn utc_time(value: &str) -> Vec<u8> {
        tlv(0x17, value.as_bytes())
    }

    pub fn explicit(number: u8, body: &[u8]) -> Vec<u8> {
        tlv(0xa0 | number, body)
    }

    pub fn null() -> Vec<u8> {
        vec![0x05, 0x00]
    }

    pub fn common_name(name: &str) -> Vec<u8> {
        seq(vec![tlv(0x31, &seq(vec![oid("2.5.4.3"), utf8(name)]))])
    }

    pub fn extension(oid_dotted: &str, critical: bool, value: Vec<u8>) -> Vec<u8> {
        let mut parts = vec![oid(oid_dotted)];
        if critical {
            parts.push(boolean(true));
        }
        parts.push(octet_string(&value));
        seq(parts)
    }
}

mod certgen {
    use covenant_compute_attestation::vendor_types::SnpTcb;
    use p384::ecdsa::SigningKey;
    use rand::rngs::OsRng;
    use rsa::pkcs8::EncodePublicKey;
    use rsa::signature::{RandomizedSigner, SignatureEncoding, Signer};
    use rsa::{RsaPrivateKey, RsaPublicKey};

    use super::der;

    #[derive(Clone, Copy)]
    pub enum Scheme {
        Pss,
        Pkcs1v15,
    }

    const SHA384: &str = "2.16.840.1.101.3.4.2.2";
    const NOT_BEFORE: &str = "260101000000Z";
    const NOT_AFTER: &str = "360101000000Z";

    fn algorithm(scheme: Scheme) -> Vec<u8> {
        match scheme {
            Scheme::Pss => der::seq(vec![
                der::oid("1.2.840.113549.1.1.10"),
                der::seq(vec![
                    der::explicit(0, &der::seq(vec![der::oid(SHA384), der::null()])),
                    der::explicit(
                        1,
                        &der::seq(vec![
                            der::oid("1.2.840.113549.1.1.8"),
                            der::seq(vec![der::oid(SHA384), der::null()]),
                        ]),
                    ),
                    der::explicit(2, &der::integer(48)),
                ]),
            ]),
            Scheme::Pkcs1v15 => der::seq(vec![der::oid("1.2.840.113549.1.1.12"), der::null()]),
        }
    }

    fn sign(scheme: Scheme, key: &RsaPrivateKey, message: &[u8]) -> Vec<u8> {
        match scheme {
            Scheme::Pss => rsa::pss::SigningKey::<sha2::Sha384>::new_with_salt_len(key.clone(), 48)
                .sign_with_rng(&mut OsRng, message)
                .to_vec(),
            Scheme::Pkcs1v15 => rsa::pkcs1v15::SigningKey::<sha2::Sha384>::new(key.clone())
                .sign(message)
                .to_vec(),
        }
    }

    fn certificate(
        spki: Vec<u8>,
        subject: &str,
        issuer: &str,
        ca: bool,
        extra_extensions: Vec<Vec<u8>>,
        issuer_key: &RsaPrivateKey,
        scheme: Scheme,
    ) -> Vec<u8> {
        let algorithm = algorithm(scheme);

        let mut extensions = Vec::new();
        if ca {
            extensions.push(der::extension(
                "2.5.29.19",
                true,
                der::seq(vec![der::boolean(true)]),
            ));
        }
        extensions.extend(extra_extensions);

        let tbs = der::seq(vec![
            der::explicit(0, &der::integer(2)),
            der::integer(1),
            algorithm.clone(),
            der::common_name(issuer),
            der::seq(vec![der::utc_time(NOT_BEFORE), der::utc_time(NOT_AFTER)]),
            der::common_name(subject),
            spki,
            der::explicit(3, &der::seq(extensions)),
        ]);

        let signature = sign(scheme, issuer_key, &tbs);
        der::seq(vec![tbs, algorithm, der::bit_string(&signature)])
    }

    fn spki(key: &RsaPublicKey) -> Vec<u8> {
        key.to_public_key_der()
            .expect("rsa spki")
            .as_bytes()
            .to_vec()
    }

    pub fn root(key: &RsaPrivateKey, name: &str) -> Vec<u8> {
        certificate(
            spki(&key.to_public_key()),
            name,
            name,
            true,
            Vec::new(),
            key,
            Scheme::Pss,
        )
    }

    pub fn intermediate(
        key: &RsaPublicKey,
        subject: &str,
        issuer: &str,
        issuer_key: &RsaPrivateKey,
        scheme: Scheme,
    ) -> Vec<u8> {
        certificate(
            spki(key),
            subject,
            issuer,
            true,
            Vec::new(),
            issuer_key,
            scheme,
        )
    }

    /// HWID as an OCTET STRING and the four SVNs as INTEGERs, which is one of
    /// the two encodings AMD ships. AMD leaves basicConstraints off the VCEK,
    /// so this does too.
    pub fn vcek(
        key: &SigningKey,
        hwid: [u8; 64],
        tcb: SnpTcb,
        issuer: &str,
        issuer_key: &RsaPrivateKey,
    ) -> Vec<u8> {
        let spki = p384::pkcs8::EncodePublicKey::to_public_key_der(key.verifying_key())
            .expect("p384 spki")
            .as_bytes()
            .to_vec();
        let extensions = vec![
            der::extension("1.3.6.1.4.1.3704.1.4", false, hwid.to_vec()),
            der::extension(
                "1.3.6.1.4.1.3704.1.3.1",
                false,
                der::tlv(0x02, &[tcb.bootloader]),
            ),
            der::extension("1.3.6.1.4.1.3704.1.3.2", false, der::tlv(0x02, &[tcb.tee])),
            der::extension("1.3.6.1.4.1.3704.1.3.3", false, der::tlv(0x02, &[tcb.snp])),
            der::extension(
                "1.3.6.1.4.1.3704.1.3.8",
                false,
                der::tlv(0x02, &[tcb.microcode]),
            ),
        ];

        certificate(
            spki,
            "SEV-VCEK-test",
            issuer,
            false,
            extensions,
            issuer_key,
            Scheme::Pss,
        )
    }
}

/// Regenerates every locally generated fixture. Ignored because it rewrites
/// checked-in files and produces fresh keys each run. Run it under `--release`:
/// it generates four RSA-4096 keys, which an unoptimised build makes painful.
#[test]
#[ignore = "rewrites checked-in fixtures"]
fn regenerate_fixtures() {
    use certgen::Scheme;
    use p384::pkcs8::EncodePrivateKey;
    use rand::rngs::OsRng;
    use rsa::RsaPrivateKey;

    const ARK: &str = "ARK-Genoa-test";
    const ASK: &str = "SEV-Genoa-test";

    let dir = fixtures();
    fs::create_dir_all(&dir).expect("fixture directory");

    let ark_key = RsaPrivateKey::new(&mut OsRng, 4096).expect("ARK key");
    let ask_key = RsaPrivateKey::new(&mut OsRng, 4096).expect("ASK key");
    let attacker_ark_key = RsaPrivateKey::new(&mut OsRng, 4096).expect("attacker ARK key");
    let attacker_ask_key = RsaPrivateKey::new(&mut OsRng, 4096).expect("attacker ASK key");
    let vcek_key = SigningKey::random(&mut OsRng);
    let attacker_vcek_key = SigningKey::random(&mut OsRng);

    let write = |name: &str, bytes: &[u8]| {
        fs::write(dir.join(name), bytes).unwrap_or_else(|error| panic!("write {name}: {error}"))
    };

    write("test-ark.der", &certgen::root(&ark_key, ARK));
    write(
        "ask.der",
        &certgen::intermediate(&ask_key.to_public_key(), ASK, ARK, &ark_key, Scheme::Pss),
    );
    write(
        "ask-pkcs1v15.der",
        &certgen::intermediate(
            &ask_key.to_public_key(),
            ASK,
            ARK,
            &ark_key,
            Scheme::Pkcs1v15,
        ),
    );
    write(
        "vcek.der",
        &certgen::vcek(&vcek_key, CHIP_ID, tcb_floor(), ASK, &ask_key),
    );
    write(
        "vcek-wrong-hwid.der",
        &certgen::vcek(&vcek_key, [0x77; 64], tcb_floor(), ASK, &ask_key),
    );
    write(
        "vcek-low-svn.der",
        &certgen::vcek(
            &vcek_key,
            CHIP_ID,
            SnpTcb {
                microcode: tcb_floor().microcode - 1,
                ..tcb_floor()
            },
            ASK,
            &ask_key,
        ),
    );
    write(
        "vcek-key.pkcs8.der",
        vcek_key.to_pkcs8_der().expect("pkcs8").as_bytes(),
    );

    write("attacker-ark.der", &certgen::root(&attacker_ark_key, ARK));
    write(
        "attacker-ask.der",
        &certgen::intermediate(
            &attacker_ask_key.to_public_key(),
            ASK,
            ARK,
            &attacker_ark_key,
            Scheme::Pss,
        ),
    );
    write(
        "attacker-vcek.der",
        &certgen::vcek(
            &attacker_vcek_key,
            CHIP_ID,
            tcb_floor(),
            ASK,
            &attacker_ask_key,
        ),
    );
    write(
        "attacker-vcek-key.pkcs8.der",
        attacker_vcek_key.to_pkcs8_der().expect("pkcs8").as_bytes(),
    );

    write("report.bin", &good_report_builder().signed_with(&vcek_key));
}

/// Writes the placeholder that stands in for AMD's Genoa ARK until the real one
/// is pinned, discarding its private key. Kept apart from `regenerate_fixtures`
/// so that regenerating the vectors can never rewrite the production root.
#[test]
#[ignore = "rewrites the pinned root"]
fn regenerate_placeholder_ark() {
    use rand::rngs::OsRng;
    use rsa::RsaPrivateKey;

    let key = RsaPrivateKey::new(&mut OsRng, 4096).expect("placeholder ARK key");
    let root = certgen::root(&key, "ARK-Genoa-placeholder");
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("roots/amd-ark-genoa.der");
    fs::write(&path, &root).expect("write placeholder ARK");

    println!(
        "roots/amd-ark-genoa.der sha256: {}",
        hex::encode(Sha256::digest(&root))
    );
}

/// The whole path on real silicon: a report taken inside a confidential guest on
/// prism-node-01, bound to a lease through REPORT_DATA and to the workload
/// through HOST_DATA, verified against the VCEK AMD issued for that chip.
///
/// Nothing here is generated. The report is what the PSP signed, the chain is
/// what AMD's KDS serves for that chip and TCB, and the launch measurement is
/// the one sev-snp-measure computes from the recorded boot inputs.
#[test]
fn a_genuine_lease_report_earns_attested() {
    let report = fs::read(fixtures().join("genuine-lease/report.bin")).expect("genuine report");
    let chain: Vec<String> = ["vcek.der", "ask.der", "ark.der"]
        .iter()
        .map(|name| {
            STANDARD.encode(fs::read(fixtures().join("genuine-lease/chain").join(name)).unwrap())
        })
        .collect();

    // Kata commits to the workload by hashing the initdata document it was
    // launched with, and the digest lands in HOST_DATA.
    let host_data: [u8; 32] =
        hex::decode("48d99906dbea27dcce229c6385afeaffb55a5e410773c2caec0cee56f5162890")
            .unwrap()
            .try_into()
            .unwrap();
    let expectation = SnpExpectation {
        report_data: report_data(),
        host_data,
        chip_id_digest: None,
    };
    let captured_at = Utc.with_ymd_and_hms(2026, 8, 17, 20, 58, 0).unwrap();

    let verdict = verify_sev_snp_attestation(
        &attestation(report, chain),
        &expectation,
        captured_at,
        &Policy::default(),
    )
    .expect("verdict");

    assert_eq!(verdict.granted_class, TrustClass::Attested);
    assert_eq!(verdict.guest.vmpl, 0);
    assert!(!verdict.guest.policy_debug);
    assert_eq!(
        verdict.guest.measurement,
        hex::encode(reference_measurement())
    );
}

/// The chip a report names has to be readable before the report is verified,
/// because that is what decides which certificate to go and fetch. The values
/// here are the ones AMD's KDS answers to for the genuine capture: chip
/// a93b917d…, TCB 7/0/23/72.
#[test]
fn a_report_names_the_chip_to_fetch_a_certificate_for() {
    let report = fs::read(fixtures().join("genuine-lease/report.bin")).expect("genuine report");
    let origin = covenant_compute_attestation::claimed_origin(&report).expect("origin");

    assert_eq!(&hex::encode(origin.chip_id)[..8], "a93b917d");
    assert_eq!(origin.reported_tcb.bootloader, 7);
    assert_eq!(origin.reported_tcb.tee, 0);
    assert_eq!(origin.reported_tcb.snp, 23);
    assert_eq!(origin.reported_tcb.microcode, 72);
}

/// Nothing about the claim is trusted, so a report that is not even the right
/// size is refused rather than producing a chip id to chase.
#[test]
fn a_malformed_report_names_no_chip() {
    assert!(covenant_compute_attestation::claimed_origin(&[0_u8; 16]).is_err());
}
