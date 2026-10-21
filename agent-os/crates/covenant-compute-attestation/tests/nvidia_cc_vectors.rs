//! Vectors for the NVIDIA CC verifier, over evidence captured from real
//! confidential silicon.
//!
//! `genuine/` is a single-GPU H100 CC report pulled from a Phala-hosted
//! confidential inference node (RedPill), 2026-08-24: driver 580.95.05, the
//! feature flag set to single-GPU passthrough, PPCIE disabled. Its nonce is
//! the provider's, not one this crate issued, and its driver is a different
//! patch than the pinned measurement reference, so it earns everything the
//! confidential rung checks except the lease-bound nonce and the driver-exact
//! measurement match, the same shape the genuine Genoa SNP capture takes.
//!
//! `report.bin` / `chain.json` (no subdir) is NVIDIA's own nvtrust sample: a
//! genuine GH100 report, but from an older driver whose opaque data predates
//! the feature flag. It proves the parser and the signature chain against real
//! silicon while proving the rung refuses a report that cannot attest its mode.

use chrono::{DateTime, TimeZone, Utc};
use covenant_compute_attestation::{Policy, VerificationError, verify_nvidia_cc_lease_attestation};

const GENUINE_REPORT: &[u8] = include_bytes!("fixtures/nvidia-cc/genuine/report.bin");
const GENUINE_CHAIN: &str = include_str!("fixtures/nvidia-cc/genuine/chain.json");
const GENUINE_NONCE: &str = "cbdf4b72ab42e74b6aaa46cff67692f15b7b70b74d08aee2bb3a8ce39f7c4650";

const NVTRUST_REPORT: &[u8] = include_bytes!("fixtures/nvidia-cc/report.bin");
const NVTRUST_CHAIN: &str = include_str!("fixtures/nvidia-cc/chain.json");
const NVTRUST_NONCE: &str = "931d8dd0add203ac3d8b4fbde75e115278eefcdceac5b87671a748f32364dfcb";

const LEASE_ID: u64 = 8123;
const NODE_ID: &str = "0xccnode";

/// Both captures' device certificates are current in 2026; pin a date inside
/// their validity so the vectors do not drift with the clock.
fn vector_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 24, 12, 0, 0).unwrap()
}

fn chain(json: &str) -> Vec<Vec<u8>> {
    use base64::Engine as _;
    let encoded: Vec<String> = serde_json::from_str(json).unwrap();
    encoded
        .iter()
        .map(|c| base64::engine::general_purpose::STANDARD.decode(c).unwrap())
        .collect()
}

fn nonce(hexed: &str) -> [u8; 32] {
    hex::decode(hexed).unwrap().try_into().unwrap()
}

fn verify(
    report: &[u8],
    chain_json: &str,
    challenge: &str,
) -> Result<covenant_compute_attestation::vendor_types::LeaseGpuCcVerdict, VerificationError> {
    verify_nvidia_cc_lease_attestation(
        LEASE_ID,
        NODE_ID,
        report,
        &chain(chain_json),
        &nonce(challenge),
        vector_time(),
        &Policy::default(),
    )
}

/// The confidentiality-critical legs all pass against real silicon: the
/// signature chains to NVIDIA's Device Identity CA, the provider's nonce is
/// bound, the leaf DICE identity matches the report FWID, the feature flag is
/// single-GPU confidential, and PPCIE is disabled. It stops only at the
/// measurement match, because this node's driver is a different patch than the
/// pinned reference. That is proof of everything the rung asserts short of a
/// driver-exact golden set.
#[test]
fn a_genuine_cc_report_proves_confidential_mode_on_real_silicon() {
    let refused = verify(GENUINE_REPORT, GENUINE_CHAIN, GENUINE_NONCE);
    assert_eq!(refused, Err(VerificationError::UnknownMeasurement));
}

/// A genuine report whose opaque data predates the feature flag cannot earn
/// the rung: the single-GPU confidential mode is exactly what it does not
/// attest, and the verifier refuses rather than inferring it from the report
/// merely existing.
#[test]
fn a_report_without_the_feature_flag_cannot_earn_confidential() {
    let refused = verify(NVTRUST_REPORT, NVTRUST_CHAIN, NVTRUST_NONCE);
    assert_eq!(refused, Err(VerificationError::NvCcModeUnproven));
}

/// The GPU signs the caller's nonce into the report, so a report taken for
/// another challenge is worthless here even though its signature is genuine.
#[test]
fn a_cc_report_answering_another_nonce_is_refused() {
    let mut wrong = nonce(GENUINE_NONCE);
    wrong[0] ^= 1;
    let refused = verify_nvidia_cc_lease_attestation(
        LEASE_ID,
        NODE_ID,
        GENUINE_REPORT,
        &chain(GENUINE_CHAIN),
        &wrong,
        vector_time(),
        &Policy::default(),
    );
    assert_eq!(refused, Err(VerificationError::NvCcNonceMismatch));
}

/// A single flipped byte in the signed body breaks the device signature.
#[test]
fn a_tampered_cc_report_is_refused() {
    let mut tampered = GENUINE_REPORT.to_vec();
    tampered[100] ^= 1;
    let refused = verify_nvidia_cc_lease_attestation(
        LEASE_ID,
        NODE_ID,
        &tampered,
        &chain(GENUINE_CHAIN),
        &nonce(GENUINE_NONCE),
        vector_time(),
        &Policy::default(),
    );
    assert_eq!(refused, Err(VerificationError::NvCcReportSignatureInvalid));
}

/// The report is signed by its own device's leaf key. Presented under another
/// device's chain, the signature does not verify: a report cannot be relayed
/// under a certificate that did not sign it.
#[test]
fn a_cc_report_under_another_devices_chain_is_refused() {
    let refused = verify_nvidia_cc_lease_attestation(
        LEASE_ID,
        NODE_ID,
        GENUINE_REPORT,
        &chain(NVTRUST_CHAIN),
        &nonce(GENUINE_NONCE),
        vector_time(),
        &Policy::default(),
    );
    assert_eq!(refused, Err(VerificationError::NvCcReportSignatureInvalid));
}

/// A chain that does not anchor at the pinned NVIDIA root earns nothing, even
/// though the report itself is genuine.
#[test]
fn a_cc_report_needs_the_pinned_nvidia_root() {
    // Drop the root; the chain no longer anchors.
    let mut truncated = chain(GENUINE_CHAIN);
    truncated.pop();
    let refused = verify_nvidia_cc_lease_attestation(
        LEASE_ID,
        NODE_ID,
        GENUINE_REPORT,
        &truncated,
        &nonce(GENUINE_NONCE),
        vector_time(),
        &Policy::default(),
    );
    assert_eq!(refused, Err(VerificationError::UntrustedRoot));
}

#[test]
fn a_truncated_cc_report_is_refused() {
    let refused = verify_nvidia_cc_lease_attestation(
        LEASE_ID,
        NODE_ID,
        &GENUINE_REPORT[..GENUINE_REPORT.len() / 2],
        &chain(GENUINE_CHAIN),
        &nonce(GENUINE_NONCE),
        vector_time(),
        &Policy::default(),
    );
    assert_eq!(refused, Err(VerificationError::MalformedEvidence));
}
