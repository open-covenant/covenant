//! Vectors over a TDX quote captured from a live CVM.
//!
//! The quote, collateral and event log were captured 2026-08-24 from a CVM we
//! deployed on Phala Cloud (dstack, teepod prod9) running a known compose
//! file, then torn down. Intel signed the quote, Intel's PCS signed the
//! collateral (fetched through Phala's PCCS mirror), and the event log is
//! what the guest agent reported; nothing is fabricated. The price of real
//! evidence is a fixed clock: verification is pinned inside the collateral's
//! validity window, and the same quote at a later clock refuses, which is
//! itself one of the things asserted below.
//!
//! `sample-quote.bin` is the genuine Intel-signed sample from Phala's
//! dcap-qvl repository, kept for the cross-platform negative: a quote from
//! one platform must refuse under another platform's collateral.

use chrono::{DateTime, TimeZone, Utc};
use covenant_compute_attestation::vendor_types::{
    AttestationKind, HostTeeCapability, NodeAttestation, TrustClass,
};
use covenant_compute_attestation::{
    Policy, TDX_VERIFIER_VERSION, TdxEvent, TdxExpectation, TdxLaunchIdentity, VerificationError,
    verify_tdx_attestation,
};

const QUOTE: &[u8] = include_bytes!("fixtures/tdx/live-quote.bin");
const COLLATERAL: &str = include_str!("fixtures/tdx/live-collateral.json");
const EVENTS: &str = include_str!("fixtures/tdx/live-events.json");
const SAMPLE_QUOTE: &[u8] = include_bytes!("fixtures/tdx/sample-quote.bin");

const MR_TD: &str = "f06dfda6dce1cf904d4e2bab1dc370634cf95cefa2ceb2de2eee127c9382698090d7a4a13e14c536ec6c9c3c8fa87077";
const RTMR0: &str = "68102e7b524af310f7b7d426ce75481e36c40f5d513a9009c046e9d37e31551f0134d954b496a3357fd61d03f07ffe96";
const RTMR1: &str = "07e6f51aa763abfe75c3ddfbf4f425fe3f0ceff66d807a75e049303dce9addf68e7218729bd419638af63a370f65878c";
const RTMR2: &str = "a2a58c9a959a4fa44bd6da0c97a2270c051faf12084cfe91ae900e4fdff6cdd4f69a82005e04ee920f231497894d677f";
const REPORT_DATA: &str = "e358fd518d38bb3cbda79bebcdfc1738873de340b4b721ec63c6f834fd3831fe821f5a17c0e741a08b413590c89a1394de95971308948cda9e06a3bad59faee3";
const COMPOSE_HASH: &str = "c0fbe230ec1ce7ad7a092b8b698181a980df8555ab47e671f5464623c567b54f";
const INSTANCE_ID: &str = "3ae8bc0689b80e022d2f3021dc467be445249cdf";

/// Inside the collateral's validity window; its TCB info runs out 2026-09-23.
fn vector_time() -> DateTime<Utc> {
    Utc.timestamp_opt(1_787_600_000, 0).unwrap()
}

fn digest48(hexed: &str) -> [u8; 48] {
    hex::decode(hexed).unwrap().try_into().unwrap()
}

fn events() -> Vec<TdxEvent> {
    let raw: Vec<serde_json::Value> = serde_json::from_str(EVENTS).unwrap();
    raw.iter()
        .map(|entry| TdxEvent {
            imr: entry["imr"].as_u64().unwrap() as u32,
            event_type: entry["event_type"].as_u64().unwrap() as u32,
            name: entry["event"].as_str().unwrap().to_string(),
            digest: hex::decode(entry["digest"].as_str().unwrap()).unwrap(),
            payload: hex::decode(entry["event_payload"].as_str().unwrap()).unwrap(),
            version: 1,
            preimage: None,
        })
        .collect()
}

fn expectation() -> TdxExpectation {
    TdxExpectation {
        report_data: hex::decode(REPORT_DATA).unwrap().try_into().unwrap(),
        compose_hash: hex::decode(COMPOSE_HASH).unwrap().try_into().unwrap(),
    }
}

fn quote_identity() -> TdxLaunchIdentity {
    TdxLaunchIdentity {
        mr_td: digest48(MR_TD),
        rtmr0: digest48(RTMR0),
        rtmr1: digest48(RTMR1),
        rtmr2: digest48(RTMR2),
    }
}

fn attestation() -> NodeAttestation {
    use base64::Engine as _;
    NodeAttestation {
        tdx_event_log: Vec::new(),
        tdx_collateral_json: None,
        node_id: "0xtdxnode".into(),
        challenge_id: uuid::Uuid::nil(),
        kind: AttestationKind::Tdx,
        evidence_base64: base64::engine::general_purpose::STANDARD.encode(QUOTE),
        certificate_chain_base64: Vec::new(),
        capability: HostTeeCapability::default(),
        pci_address: String::new(),
        collected_at: vector_time(),
        signature: String::new(),
    }
}

fn accepting_policy() -> Policy {
    Policy::for_tests().with_tdx_test_identities(vec![quote_identity()])
}

#[test]
fn a_live_quote_with_its_log_verifies_and_earns_attested() {
    let verdict = verify_tdx_attestation(
        &attestation(),
        COLLATERAL,
        &events(),
        &expectation(),
        vector_time(),
        &accepting_policy(),
    )
    .expect("captured quote");

    assert_eq!(verdict.kind, AttestationKind::Tdx);
    assert_eq!(verdict.granted_class, TrustClass::Attested);
    assert_eq!(verdict.verifier_version, TDX_VERIFIER_VERSION);
    assert_eq!(verdict.device_identity, format!("tdx/{INSTANCE_ID}"));
    assert_eq!(verdict.node_id, "0xtdxnode");
}

/// The guest agent's own shape: a runtime event carries its name and payload
/// but leaves the digest empty, and the verifier derives it. Blanking every
/// runtime-event digest is exactly what prismd receives over the socket, and
/// it must verify identically to the cloud shape where the field is filled.
#[test]
fn the_guest_agent_shape_with_derived_digests_verifies() {
    const DSTACK_RUNTIME_EVENT_TYPE: u32 = 0x0800_0001;
    let mut events = events();
    for event in &mut events {
        if event.event_type == DSTACK_RUNTIME_EVENT_TYPE {
            event.digest.clear();
        }
    }
    let verdict = verify_tdx_attestation(
        &attestation(),
        COLLATERAL,
        &events,
        &expectation(),
        vector_time(),
        &accepting_policy(),
    )
    .expect("the derived-digest shape verifies");
    assert_eq!(verdict.device_identity, format!("tdx/{INSTANCE_ID}"));
}

/// A tampered payload is caught even in the derived-digest shape, and by the
/// fold rather than a carried-digest comparison: the recomputed digest no
/// longer folds to the quoted register.
#[test]
fn a_tampered_payload_breaks_the_fold_when_the_digest_is_derived() {
    const DSTACK_RUNTIME_EVENT_TYPE: u32 = 0x0800_0001;
    let mut events = events();
    for event in &mut events {
        if event.event_type == DSTACK_RUNTIME_EVENT_TYPE {
            event.digest.clear();
        }
    }
    let compose = events
        .iter_mut()
        .find(|event| event.name == "compose-hash")
        .unwrap();
    compose.payload[0] ^= 1;
    let refused = verify_tdx_attestation(
        &attestation(),
        COLLATERAL,
        &events,
        &expectation(),
        vector_time(),
        &accepting_policy(),
    );
    assert_eq!(refused, Err(VerificationError::TdxEventLogMismatch));
}

/// The same quote after the collateral's validity refuses. Stale collateral
/// is indistinguishable from collateral chosen to hide a revocation, so
/// there is no grace here.
#[test]
fn stale_collateral_refuses_the_quote() {
    let later = Utc.timestamp_opt(1_795_000_000, 0).unwrap();
    let refused = verify_tdx_attestation(
        &attestation(),
        COLLATERAL,
        &events(),
        &expectation(),
        later,
        &accepting_policy(),
    );
    assert_eq!(refused, Err(VerificationError::TdxQuoteRejected));
}

#[test]
fn report_data_binds_the_challenge() {
    let mut expected = expectation();
    expected.report_data[0] ^= 1;
    let refused = verify_tdx_attestation(
        &attestation(),
        COLLATERAL,
        &events(),
        &expected,
        vector_time(),
        &accepting_policy(),
    );
    assert_eq!(refused, Err(VerificationError::TdxReportDataMismatch));
}

/// The compiled reference set accepts this quote with nothing injected,
/// because the identity it presents is the one reference/tdx-launch-
/// measurements.json records: computed by dstack-mr from the published
/// image, equal to what the live platform reported. The reference file and
/// this vector hold each other in place.
#[test]
fn the_compiled_reference_accepts_the_reproduced_identity() {
    let verdict = verify_tdx_attestation(
        &attestation(),
        COLLATERAL,
        &events(),
        &expectation(),
        vector_time(),
        &Policy::for_tests(),
    )
    .expect("the reproduced identity is on file");
    assert_eq!(verdict.granted_class, TrustClass::Attested);
}

#[test]
fn a_launch_identity_matches_as_a_whole_or_not_at_all() {
    let mut identity = quote_identity();
    identity.rtmr1[0] ^= 1;
    let refused = verify_tdx_attestation(
        &attestation(),
        COLLATERAL,
        &events(),
        &expectation(),
        vector_time(),
        &Policy::for_tests().with_tdx_test_identities(vec![identity]),
    );
    assert_eq!(refused, Err(VerificationError::TdxUnknownLaunchMeasurement));
}

#[test]
fn the_log_must_bind_the_compose_file_the_caller_expected() {
    let mut expected = expectation();
    expected.compose_hash[0] ^= 1;
    let refused = verify_tdx_attestation(
        &attestation(),
        COLLATERAL,
        &events(),
        &expected,
        vector_time(),
        &accepting_policy(),
    );
    assert_eq!(refused, Err(VerificationError::TdxComposeHashMismatch));
}

/// A payload cannot ride on the digest of a different claim: the digest that
/// was folded is authentic, so a reworded payload no longer matches it.
#[test]
fn a_tampered_event_payload_is_caught_by_its_own_digest() {
    let mut tampered = events();
    let compose = tampered
        .iter_mut()
        .find(|event| event.name == "compose-hash")
        .unwrap();
    compose.payload[0] ^= 1;
    let refused = verify_tdx_attestation(
        &attestation(),
        COLLATERAL,
        &tampered,
        &expectation(),
        vector_time(),
        &accepting_policy(),
    );
    assert_eq!(refused, Err(VerificationError::TdxEventDigestMismatch));
}

/// Dropping an event breaks the fold: the log must account for exactly what
/// the TD extended, nothing missing and nothing invented.
#[test]
fn an_incomplete_log_does_not_fold_to_the_quoted_registers() {
    let mut incomplete = events();
    let position = incomplete
        .iter()
        .position(|event| event.name == "instance-id")
        .unwrap();
    incomplete.remove(position);
    let refused = verify_tdx_attestation(
        &attestation(),
        COLLATERAL,
        &incomplete,
        &expectation(),
        vector_time(),
        &accepting_policy(),
    );
    assert_eq!(refused, Err(VerificationError::TdxEventLogMismatch));
}

/// A quote from one platform under another platform's collateral refuses:
/// collateral is per-platform, not a bearer instrument.
#[test]
fn foreign_collateral_refuses_a_quote_from_another_platform() {
    use base64::Engine as _;
    let mut foreign = attestation();
    foreign.evidence_base64 = base64::engine::general_purpose::STANDARD.encode(SAMPLE_QUOTE);
    let refused = verify_tdx_attestation(
        &foreign,
        COLLATERAL,
        &events(),
        &expectation(),
        vector_time(),
        &accepting_policy(),
    );
    assert_eq!(refused, Err(VerificationError::TdxQuoteRejected));
}

#[test]
fn a_truncated_quote_is_rejected() {
    use base64::Engine as _;
    let mut attestation = attestation();
    attestation.evidence_base64 =
        base64::engine::general_purpose::STANDARD.encode(&QUOTE[..QUOTE.len() / 2]);
    let refused = verify_tdx_attestation(
        &attestation,
        COLLATERAL,
        &events(),
        &expectation(),
        vector_time(),
        &accepting_policy(),
    );
    assert_eq!(refused, Err(VerificationError::TdxQuoteRejected));
}

#[test]
fn malformed_collateral_is_its_own_refusal() {
    let refused = verify_tdx_attestation(
        &attestation(),
        "{\"not\": \"collateral\"}",
        &events(),
        &expectation(),
        vector_time(),
        &accepting_policy(),
    );
    assert_eq!(refused, Err(VerificationError::TdxCollateralMalformed));
}

#[test]
fn evidence_of_another_kind_is_malformed_here() {
    let mut attestation = attestation();
    attestation.kind = AttestationKind::SevSnp;
    let refused = verify_tdx_attestation(
        &attestation,
        COLLATERAL,
        &events(),
        &expectation(),
        vector_time(),
        &accepting_policy(),
    );
    assert_eq!(refused, Err(VerificationError::MalformedEvidence));
}
