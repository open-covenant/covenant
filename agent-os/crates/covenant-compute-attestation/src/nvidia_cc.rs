//! Verification of a genuine NVIDIA Hopper confidential-computing attestation.
//!
//! This reads the real report an H100/H200 produces: an SPDM GET_MEASUREMENTS
//! request concatenated with the signed response, plus the device certificate
//! chain. It is not the internal report format the isolated-tier GPU verifier
//! parses; it is the vendor-native evidence NVIDIA's own tooling and NRAS
//! consume, so a report captured from real silicon verifies here unchanged.
//!
//! What a verified report proves: the response was signed by a key that chains
//! to NVIDIA's Device Identity CA (genuine silicon), it answers the caller's
//! nonce (fresh, not replayed), the firmware measurements match the pinned
//! reference, and the leaf certificate's DICE identity matches the firmware
//! the report carries. What earns Confidential on top of that is the report's
//! own confidential-mode flag: a single-GPU CC passthrough mode, read from the
//! opaque feature flag the GPU signs, not inferred from the mere existence of
//! a report.

use p384::ecdsa::{Signature, signature::Verifier};
use p384::elliptic_curve::subtle::ConstantTimeEq;
use sha2::{Digest, Sha256};
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::policy::Policy;
use crate::{VerificationError, chain};

/// The device chain is leaf, three intermediates, root. One more than the GPU
/// device-report chain, because this is the full DICE path to the identity CA.
const CC_CHAIN_CERTIFICATES: usize = 5;

/// SPDM P-384 signatures are raw r||s, 48 bytes each.
const SPDM_SIGNATURE_LEN: usize = 96;

/// The report is `request || response`. The request is a fixed-size
/// GET_MEASUREMENTS message and the caller's nonce lives inside it, which is
/// why the nonce sits at a fixed low offset rather than after the record.
const REQUEST_LEN: usize = 37;
const REQUEST_NONCE: core::ops::Range<usize> = 4..36;

/// GET_MEASUREMENTS request and MEASUREMENTS response codes, checked so a
/// truncated or mis-typed capture is rejected before anything is trusted.
const GET_MEASUREMENTS_REQUEST: u8 = 0xe0;
const MEASUREMENTS_RESPONSE: u8 = 0x60;

/// Opaque field ids, from NVIDIA's SPDM measurement response layout.
const OPAQUE_FWID: u16 = 20;
const OPAQUE_PROTECTED_PCIE_STATUS: u16 = 21;
const OPAQUE_FEATURE_FLAG: u16 = 36;

/// The confidential feature flag values. Single- and multi-passthrough are the
/// single-GPU confidential modes; PPCIE is the multi-GPU protected fabric,
/// which this rung does not grant because its guarantee is a different one.
const FEATURE_FLAG_SPT: u8 = 0;
const FEATURE_FLAG_MPT: u8 = 1;

/// PPCIE/multi-GPU status uses NVIDIA's enabled/disabled convention. A
/// single-GPU confidential report has it disabled.
const PROTECTED_PCIE_DISABLED: u8 = 0x55;

/// The DICE FWID lives in one of these certificate extensions (the multi-TcbInfo
/// form and the single form); the last 48 bytes of its value are the SHA-384
/// the report must also carry.
const DICE_FWID_OIDS: [&str; 2] = ["2.23.133.5.4.1.1", "2.23.133.5.4.1"];
const FWID_LEN: usize = 48;

struct SpdmReport<'a> {
    /// The bytes the signature covers: everything but the trailing signature.
    signed: &'a [u8],
    signature: &'a [u8],
    request_nonce: [u8; 32],
    measurements: Vec<(u32, [u8; 48])>,
    fwid: [u8; 48],
    feature_flag: Option<u8>,
    protected_pcie: Option<u8>,
}

/// Verifies one NVIDIA CC attestation and, on success, returns the class it
/// earns for the lease it is bound to.
///
/// `expected_nonce` is the lease challenge the control plane issued. The GPU
/// signs the caller's nonce into the request half of the report, so comparing
/// it here is what makes a report captured for another lease, or replayed,
/// worthless. The verdict is lease-scoped for the same reason a guest report
/// is: it speaks for one session's GPU, not for the node.
pub fn verify_nvidia_cc_attestation(
    report_bytes: &[u8],
    certificate_chain: &[Vec<u8>],
    expected_nonce: &[u8; 32],
    now: chrono::DateTime<chrono::Utc>,
    policy: &Policy,
) -> Result<CcVerdict, VerificationError> {
    let verified = chain::verify_chain(certificate_chain, CC_CHAIN_CERTIFICATES, now, policy)?;
    let report = parse_report(report_bytes)?;

    let signature = Signature::from_slice(report.signature)
        .map_err(|_| VerificationError::NvCcReportSignatureInvalid)?;
    verified
        .leaf_public_key
        .verify(report.signed, &signature)
        .map_err(|_| VerificationError::NvCcReportSignatureInvalid)?;

    if !bool::from(report.request_nonce.ct_eq(expected_nonce)) {
        return Err(VerificationError::NvCcNonceMismatch);
    }

    // The leaf certificate's DICE identity has to be the firmware the report
    // measured, or a report could be presented under a certificate from a
    // different device state.
    let leaf_fwid = leaf_dice_fwid(&certificate_chain[0])?;
    if !bool::from(leaf_fwid.ct_eq(&report.fwid)) {
        return Err(VerificationError::NvCcFwidMismatch);
    }

    // Confidential mode is a property the GPU signs, not one inferred from the
    // report existing. A report without the feature flag cannot earn this rung
    // however genuine it is: the single-GPU CC state is exactly what it does
    // not attest.
    match report.feature_flag {
        Some(FEATURE_FLAG_SPT | FEATURE_FLAG_MPT) => {}
        Some(_) => return Err(VerificationError::NvCcNotSingleGpuConfidential),
        None => return Err(VerificationError::NvCcModeUnproven),
    }
    // A single-GPU confidential report must not also claim the multi-GPU
    // protected fabric, whose guarantee this rung is not verifying.
    if report.protected_pcie != Some(PROTECTED_PCIE_DISABLED) {
        return Err(VerificationError::NvCcNotSingleGpuConfidential);
    }

    check_measurements(&report.measurements)?;

    Ok(CcVerdict {
        device_identity: format!("{}/{}", verified.leaf_common_name, hex::encode(report.fwid)),
        measurement_digest: measurement_digest(&report.measurements),
    })
}

/// What a verified CC report yields the control plane, before it is stamped
/// into a lease verdict. The class is fixed at the call site so this crate
/// keeps saying what evidence earns and the protocol keeps saying what the
/// network serves.
pub struct CcVerdict {
    pub device_identity: String,
    pub measurement_digest: String,
}

fn parse_report(data: &[u8]) -> Result<SpdmReport<'_>, VerificationError> {
    let signed_len = data
        .len()
        .checked_sub(SPDM_SIGNATURE_LEN)
        .ok_or(VerificationError::MalformedEvidence)?;
    if signed_len <= REQUEST_LEN {
        return Err(VerificationError::MalformedEvidence);
    }
    let (signed, signature) = data.split_at(signed_len);

    if signed[1] != GET_MEASUREMENTS_REQUEST {
        return Err(VerificationError::MalformedEvidence);
    }
    let mut request_nonce = [0u8; 32];
    request_nonce.copy_from_slice(&signed[REQUEST_NONCE]);

    let response = &signed[REQUEST_LEN..];
    // response: version, code, param1, param2, number_of_blocks, record_len(3),
    // record, responder_nonce(32), opaque_len(2), opaque.
    if response.len() < 8 || response[1] != MEASUREMENTS_RESPONSE {
        return Err(VerificationError::MalformedEvidence);
    }
    let record_len = u24_le(&response[5..8]) as usize;
    let record_end = 8usize
        .checked_add(record_len)
        .filter(|end| *end <= response.len())
        .ok_or(VerificationError::MalformedEvidence)?;
    let measurements = parse_measurement_blocks(&response[8..record_end])?;

    let after_record = &response[record_end..];
    // responder nonce (32) then opaque length (2).
    let opaque_len_at = 32usize;
    let opaque_at = opaque_len_at + 2;
    if after_record.len() < opaque_at {
        return Err(VerificationError::MalformedEvidence);
    }
    let opaque_len = u16_le(&after_record[opaque_len_at..opaque_at]) as usize;
    let opaque_end = opaque_at
        .checked_add(opaque_len)
        .filter(|end| *end <= after_record.len())
        .ok_or(VerificationError::MalformedEvidence)?;
    let opaque = parse_opaque(&after_record[opaque_at..opaque_end])?;

    Ok(SpdmReport {
        signed,
        signature,
        request_nonce,
        measurements,
        fwid: opaque.fwid.ok_or(VerificationError::NvCcFwidMismatch)?,
        feature_flag: opaque.feature_flag,
        protected_pcie: opaque.protected_pcie,
    })
}

/// Each block is index, spec (must be DMTF), a two-byte size, then a DMTF
/// measurement whose value is a 48-byte SHA-384. Only active, non-zero blocks
/// are carried out; the all-zero blocks NVIDIA emits for inactive slots are
/// not measurements to check.
fn parse_measurement_blocks(mut record: &[u8]) -> Result<Vec<(u32, [u8; 48])>, VerificationError> {
    const DMTF_SPEC: u8 = 1;
    const DMTF_SHA384_SIZE: usize = 48;
    let mut out = Vec::new();
    while !record.is_empty() {
        if record.len() < 4 {
            return Err(VerificationError::MalformedEvidence);
        }
        let index = record[0] as u32;
        if record[1] != DMTF_SPEC {
            return Err(VerificationError::MalformedEvidence);
        }
        let size = u16_le(&record[2..4]) as usize;
        let value_end = 4usize
            .checked_add(size)
            .filter(|end| *end <= record.len())
            .ok_or(VerificationError::MalformedEvidence)?;
        let value = &record[4..value_end];
        // DMTF measurement: value type (1), value size (2), value.
        if value.len() >= 3 {
            let inner = u16_le(&value[1..3]) as usize;
            if inner == DMTF_SHA384_SIZE && value.len() >= 3 + DMTF_SHA384_SIZE {
                let digest: [u8; 48] = value[3..3 + DMTF_SHA384_SIZE]
                    .try_into()
                    .expect("checked length");
                if digest.iter().any(|byte| *byte != 0) {
                    out.push((index, digest));
                }
            }
        }
        record = &record[value_end..];
    }
    Ok(out)
}

#[derive(Default)]
struct Opaque {
    fwid: Option<[u8; 48]>,
    feature_flag: Option<u8>,
    protected_pcie: Option<u8>,
}

fn parse_opaque(mut data: &[u8]) -> Result<Opaque, VerificationError> {
    let mut opaque = Opaque::default();
    while !data.is_empty() {
        if data.len() < 4 {
            return Err(VerificationError::MalformedEvidence);
        }
        let field = u16_le(&data[0..2]);
        let size = u16_le(&data[2..4]) as usize;
        let end = 4usize
            .checked_add(size)
            .filter(|end| *end <= data.len())
            .ok_or(VerificationError::MalformedEvidence)?;
        let value = &data[4..end];
        match field {
            OPAQUE_FWID if value.len() == FWID_LEN => {
                opaque.fwid = Some(value.try_into().expect("checked length"));
            }
            OPAQUE_FEATURE_FLAG if !value.is_empty() => opaque.feature_flag = Some(value[0]),
            OPAQUE_PROTECTED_PCIE_STATUS if !value.is_empty() => {
                opaque.protected_pcie = Some(value[0]);
            }
            _ => {}
        }
        data = &data[end..];
    }
    Ok(opaque)
}

/// The DICE FWID extension carries the firmware identity; its last 48 bytes are
/// the SHA-384 the report echoes in its opaque data.
fn leaf_dice_fwid(leaf_der: &[u8]) -> Result<[u8; 48], VerificationError> {
    let (_, certificate) =
        X509Certificate::from_der(leaf_der).map_err(|_| VerificationError::MalformedCertificate)?;
    let extension = certificate
        .extensions()
        .iter()
        .find(|extension| DICE_FWID_OIDS.contains(&extension.oid.to_id_string().as_str()))
        .ok_or(VerificationError::NvCcFwidMismatch)?;
    let value = extension.value;
    if value.len() < FWID_LEN {
        return Err(VerificationError::NvCcFwidMismatch);
    }
    Ok(value[value.len() - FWID_LEN..]
        .try_into()
        .expect("checked length"))
}

/// Every reported measurement has to be a value the reference set allows for
/// its slot. A report that carries a measurement the reference does not know is
/// as rejected as one that answers the wrong nonce.
fn check_measurements(measurements: &[(u32, [u8; 48])]) -> Result<(), VerificationError> {
    if measurements.is_empty() {
        return Err(VerificationError::UnknownMeasurement);
    }
    let allowlist = crate::policy::measurement_allowlist();
    for (index, digest) in measurements {
        match allowlist.accepts(*index, digest) {
            Some(true) => {}
            _ => return Err(VerificationError::UnknownMeasurement),
        }
    }
    Ok(())
}

fn measurement_digest(measurements: &[(u32, [u8; 48])]) -> String {
    let mut sorted: Vec<&(u32, [u8; 48])> = measurements.iter().collect();
    sorted.sort_by_key(|(index, _)| *index);
    let mut hasher = Sha256::new();
    hasher.update(b"prism-attestation/nvidia-cc-measurements/1\n");
    for (index, digest) in sorted {
        hasher.update(format!("{index} {}\n", hex::encode(digest)));
    }
    hex::encode(hasher.finalize())
}

fn u16_le(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn u24_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0])
}
