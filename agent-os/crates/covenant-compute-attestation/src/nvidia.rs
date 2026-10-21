use p384::ecdsa::{Signature, VerifyingKey, signature::Verifier};

use crate::VerificationError;

/// Wire layout of the device report the node forwards, mirroring the fields
/// Prism consumes from an SPDM measurement response: fixed header, the
/// measurement records, then a raw r||s signature over everything before it.
/// The magic and version let a capture from a newer driver be rejected instead
/// of misread.
const REPORT_MAGIC: &[u8; 4] = b"NVAR";
const REPORT_VERSION: u16 = 1;
const HEADER_LEN: usize = 64;
const MEASUREMENT_LEN: usize = 52;
/// A report identifies a measurement by its slot index, the way NVIDIA's
/// reference manifest does. Keying on a name instead meant carrying a
/// fixed-width string, and a name longer than the field ran over the digest
/// beside it.
const MEASUREMENT_INDEX_LEN: usize = 4;
const SIGNATURE_LEN: usize = 96;

/// A single report may not carry more measurement records than this; the cap
/// keeps a malformed length field from being read as an enormous allocation.
const MAX_MEASUREMENTS: usize = 64;

pub(crate) const H100_VENDOR_ID: u16 = 0x10de;
pub(crate) const H100_DEVICE_ID: u16 = 0x2331;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CcMode {
    Off,
    On,
    DevTools,
}

impl CcMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
            Self::DevTools => "devtools",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Measurement {
    pub(crate) index: u32,
    pub(crate) digest: [u8; 48],
}

#[derive(Debug, Clone)]
pub(crate) struct Report {
    pub(crate) vendor_id: u16,
    pub(crate) device_id: u16,
    pub(crate) cc_mode: CcMode,
    pub(crate) nonce: [u8; 32],
    pub(crate) board_serial: String,
    pub(crate) measurements: Vec<Measurement>,
}

pub(crate) fn parse_report(evidence: &[u8]) -> Result<Report, VerificationError> {
    let body = signed_body(evidence)?;
    if body.len() < HEADER_LEN {
        return Err(VerificationError::MalformedEvidence);
    }
    if &body[0..4] != REPORT_MAGIC {
        return Err(VerificationError::MalformedEvidence);
    }
    if u16_at(body, 4) != REPORT_VERSION {
        return Err(VerificationError::MalformedEvidence);
    }

    let cc_mode = match body[10] {
        0 => CcMode::Off,
        1 => CcMode::On,
        2 => CcMode::DevTools,
        _ => return Err(VerificationError::MalformedEvidence),
    };
    let count = body[11] as usize;
    if count == 0 || count > MAX_MEASUREMENTS {
        return Err(VerificationError::MalformedEvidence);
    }
    if body.len() != HEADER_LEN + count * MEASUREMENT_LEN {
        return Err(VerificationError::MalformedEvidence);
    }

    let mut nonce = [0u8; 32];
    nonce.copy_from_slice(&body[12..44]);

    let mut measurements = Vec::with_capacity(count);
    for index in 0..count {
        let start = HEADER_LEN + index * MEASUREMENT_LEN;
        let record = &body[start..start + MEASUREMENT_LEN];
        let mut digest = [0u8; 48];
        digest.copy_from_slice(&record[MEASUREMENT_INDEX_LEN..]);
        let mut slot = [0u8; 4];
        slot.copy_from_slice(&record[..MEASUREMENT_INDEX_LEN]);
        measurements.push(Measurement {
            index: u32::from_le_bytes(slot),
            digest,
        });
    }

    Ok(Report {
        vendor_id: u16_at(body, 6),
        device_id: u16_at(body, 8),
        cc_mode,
        nonce,
        board_serial: ascii_field(&body[44..60])?,
        measurements,
    })
}

/// The device key is the leaf of the chain that was just anchored, so a report
/// that verifies here was produced by that physical GPU and not replayed from
/// another one.
pub(crate) fn verify_report_signature(
    evidence: &[u8],
    device_key: &VerifyingKey,
) -> Result<(), VerificationError> {
    let body = signed_body(evidence)?;
    let signature = Signature::from_slice(&evidence[body.len()..])
        .map_err(|_| VerificationError::ReportSignatureInvalid)?;
    device_key
        .verify(body, &signature)
        .map_err(|_| VerificationError::ReportSignatureInvalid)
}

fn signed_body(evidence: &[u8]) -> Result<&[u8], VerificationError> {
    evidence
        .len()
        .checked_sub(SIGNATURE_LEN)
        .filter(|body_len| *body_len >= HEADER_LEN)
        .map(|body_len| &evidence[..body_len])
        .ok_or(VerificationError::MalformedEvidence)
}

fn u16_at(body: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([body[offset], body[offset + 1]])
}

/// NUL padded ASCII. Anything outside printable ASCII would end up in a verdict
/// string that other services log and compare, so it is rejected here.
fn ascii_field(raw: &[u8]) -> Result<String, VerificationError> {
    let end = raw.iter().position(|byte| *byte == 0).unwrap_or(raw.len());
    let field = &raw[..end];
    if field.is_empty() || field.iter().any(|byte| !byte.is_ascii_graphic()) {
        return Err(VerificationError::MalformedEvidence);
    }
    Ok(String::from_utf8_lossy(field).into_owned())
}

/// Builds reports in the layout the parser expects, so the vectors exercise the
/// real decoder instead of a second copy of it kept in the tests.
#[cfg(any(test, feature = "test-vectors"))]
#[derive(Debug, Clone)]
pub struct ReportBuilder {
    vendor_id: u16,
    device_id: u16,
    cc_mode: u8,
    nonce: [u8; 32],
    board_serial: String,
    measurements: Vec<(u32, [u8; 48])>,
}

#[cfg(any(test, feature = "test-vectors"))]
impl ReportBuilder {
    pub fn h100(nonce: [u8; 32], board_serial: &str) -> Self {
        Self {
            vendor_id: H100_VENDOR_ID,
            device_id: H100_DEVICE_ID,
            cc_mode: 0,
            nonce,
            board_serial: board_serial.to_string(),
            measurements: Vec::new(),
        }
    }

    pub fn device_id(mut self, device_id: u16) -> Self {
        self.device_id = device_id;
        self
    }

    pub fn vendor_id(mut self, vendor_id: u16) -> Self {
        self.vendor_id = vendor_id;
        self
    }

    pub fn cc_mode(mut self, cc_mode: u8) -> Self {
        self.cc_mode = cc_mode;
        self
    }

    pub fn nonce(mut self, nonce: [u8; 32]) -> Self {
        self.nonce = nonce;
        self
    }

    pub fn measurement(mut self, index: u32, digest: [u8; 48]) -> Self {
        self.measurements.push((index, digest));
        self
    }

    pub fn body(&self) -> Vec<u8> {
        let mut body = vec![0u8; HEADER_LEN];
        body[0..4].copy_from_slice(REPORT_MAGIC);
        body[4..6].copy_from_slice(&REPORT_VERSION.to_le_bytes());
        body[6..8].copy_from_slice(&self.vendor_id.to_le_bytes());
        body[8..10].copy_from_slice(&self.device_id.to_le_bytes());
        body[10] = self.cc_mode;
        body[11] = self.measurements.len() as u8;
        body[12..44].copy_from_slice(&self.nonce);
        let serial = self.board_serial.as_bytes();
        body[44..44 + serial.len()].copy_from_slice(serial);

        for (index, digest) in &self.measurements {
            let mut record = [0u8; MEASUREMENT_LEN];
            record[..MEASUREMENT_INDEX_LEN].copy_from_slice(&index.to_le_bytes());
            record[MEASUREMENT_INDEX_LEN..].copy_from_slice(digest);
            body.extend_from_slice(&record);
        }
        body
    }

    pub fn signed_with(&self, device_key: &p384::ecdsa::SigningKey) -> Vec<u8> {
        use p384::ecdsa::signature::Signer;

        let body = self.body();
        let signature: Signature = device_key.sign(&body);
        let mut evidence = body;
        evidence.extend_from_slice(&signature.to_bytes());
        evidence
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_truncated_report_is_rejected() {
        assert!(matches!(
            parse_report(&[0u8; 32]),
            Err(VerificationError::MalformedEvidence)
        ));
        assert!(matches!(
            parse_report(&[]),
            Err(VerificationError::MalformedEvidence)
        ));
    }

    #[test]
    fn a_report_without_the_magic_is_rejected() {
        let evidence = vec![0u8; HEADER_LEN + MEASUREMENT_LEN + SIGNATURE_LEN];
        assert!(matches!(
            parse_report(&evidence),
            Err(VerificationError::MalformedEvidence)
        ));
    }
}
