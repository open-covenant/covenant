//! Decoding of the SEV-SNP ATTESTATION_REPORT.
//!
//! Offsets, widths and bit positions are taken from the AMD SEV-SNP Firmware
//! ABI Specification, publication 56860, revision 1.55, tables
//! "ATTESTATION_REPORT Structure", "TCB_VERSION" and "GUEST_POLICY". That is
//! the ABI the Dallas platform reports (SEV-SNP API 1.55) and it defines report
//! VERSION 2. Revision 1.56 introduced VERSION 3, which only names three bytes
//! the reserved block at 0x188 used to cover. Both are accepted because every
//! field read below is identical in the two, and the platform emits 3.
//!
//! The full VERSION 2 layout, for confirming the constants below against the
//! table:
//!
//! ```text
//! 0x000 u32   VERSION              0x140 [u8; 32] REPORT_ID
//! 0x004 u32   GUEST_SVN            0x160 [u8; 32] REPORT_ID_MA
//! 0x008 u64   POLICY               0x180 u64      REPORTED_TCB
//! 0x010 [16]  FAMILY_ID            0x188 [u8; 24] reserved
//! 0x020 [16]  IMAGE_ID             0x1A0 [u8; 64] CHIP_ID
//! 0x030 u32   VMPL                 0x1E0 u64      COMMITTED_TCB
//! 0x034 u32   SIGNATURE_ALGO       0x1E8 [u8; 8]  current/committed build
//! 0x038 u64   CURRENT_TCB          0x1F0 u64      LAUNCH_TCB
//! 0x040 u64   PLATFORM_INFO        0x1F8 [u8;168] reserved
//! 0x048 u32   flags                0x2A0 [u8; 72] signature r
//! 0x04C u32   reserved             0x2E8 [u8; 72] signature s
//! 0x050 [64]  REPORT_DATA          0x330 [u8;368] reserved
//! 0x090 [48]  MEASUREMENT
//! 0x0C0 [32]  HOST_DATA
//! 0x0E0 [48]  ID_KEY_DIGEST
//! 0x110 [48]  AUTHOR_KEY_DIGEST
//! ```

use crate::vendor_types::SnpTcb;
use p384::ecdsa::{Signature, VerifyingKey, signature::Verifier};

use crate::VerificationError;

/// A report is a fixed-size structure. Accepting a prefix would mean verifying
/// a signature over bytes whose tail the sender chose, so anything but the
/// exact size is rejected before a single field is read.
pub(crate) const REPORT_LEN: usize = 1184;

/// 0x000 through 0x29F. The signature at 0x2A0 covers everything before it and
/// nothing after it.
const SIGNED_LEN: usize = 0x2A0;

/// Revision 1.55 defines VERSION 2 and 1.56 defines VERSION 3. The two are
/// identical across every field this decoder reads: 3 only names three bytes
/// at 0x188 that 2 left reserved (the CPUID family/model/stepping), and
/// nothing here depends on them. Genoa platforms in the field emit 3, so
/// refusing it rejects genuine current hardware.
const REPORT_VERSIONS: [u32; 2] = [2, 3];
#[cfg(any(test, feature = "test-vectors"))]
const REPORT_VERSION_EMITTED: u32 = 2;
const SIGNATURE_ALGO_ECDSA_P384_SHA384: u32 = 1;

/// Bits 2 through 4 of the flags word name the key that signed the report. Zero
/// is the VCEK, which is the only chain this verifier walks.
const SIGNING_KEY_VCEK: u32 = 0;

const OFF_VERSION: usize = 0x000;
const OFF_POLICY: usize = 0x008;
const OFF_VMPL: usize = 0x030;
const OFF_SIGNATURE_ALGO: usize = 0x034;
const OFF_CURRENT_TCB: usize = 0x038;
const OFF_FLAGS: usize = 0x048;
const OFF_REPORT_DATA: usize = 0x050;
const OFF_MEASUREMENT: usize = 0x090;
const OFF_HOST_DATA: usize = 0x0C0;
const OFF_REPORTED_TCB: usize = 0x180;
const OFF_CHIP_ID: usize = 0x1A0;
const OFF_COMMITTED_TCB: usize = 0x1E0;
const OFF_LAUNCH_TCB: usize = 0x1F0;
const OFF_SIGNATURE_R: usize = 0x2A0;
const OFF_SIGNATURE_S: usize = 0x2E8;

/// r and s each occupy 72 bytes little-endian in the report, of which a P-384
/// scalar can only fill the low 48.
const SIGNATURE_FIELD_LEN: usize = 72;
const SCALAR_LEN: usize = 48;

/// The TCB encoding is per product line. Genoa packs the four components at
/// bytes 0, 1, 6 and 7; Turin repacked them, so a second line has to be named
/// here and given its own floor rather than folded into this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProductLine {
    Genoa,
}

impl ProductLine {
    pub(crate) fn parse(name: &str) -> Option<Self> {
        match name {
            "genoa" => Some(Self::Genoa),
            _ => None,
        }
    }

    fn decode_tcb(self, raw: u64) -> SnpTcb {
        match self {
            Self::Genoa => SnpTcb {
                bootloader: raw as u8,
                tee: (raw >> 8) as u8,
                snp: (raw >> 48) as u8,
                microcode: (raw >> 56) as u8,
            },
        }
    }
}

/// True when every component is at or above the floor. Compared component by
/// component because the packed word is not ordered: a microcode bump would
/// otherwise mask a bootloader downgrade.
pub(crate) fn tcb_at_least(tcb: &SnpTcb, floor: &SnpTcb) -> bool {
    tcb.bootloader >= floor.bootloader
        && tcb.tee >= floor.tee
        && tcb.snp >= floor.snp
        && tcb.microcode >= floor.microcode
}

/// The four launch policy bits this verifier reads. The host chooses them at
/// SNP_LAUNCH and cannot change them afterwards, so they say what the VM was
/// started as rather than what it is claimed to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GuestPolicy(u64);

impl GuestPolicy {
    pub(crate) fn debug(self) -> bool {
        self.0 & (1 << 19) != 0
    }

    pub(crate) fn migrate_ma(self) -> bool {
        self.0 & (1 << 18) != 0
    }

    pub(crate) fn smt(self) -> bool {
        self.0 & (1 << 16) != 0
    }

    pub(crate) fn single_socket(self) -> bool {
        self.0 & (1 << 20) != 0
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Report {
    pub(crate) policy: GuestPolicy,
    pub(crate) vmpl: u32,
    pub(crate) current_tcb: SnpTcb,
    pub(crate) report_data: [u8; 64],
    pub(crate) measurement: [u8; 48],
    pub(crate) host_data: [u8; 32],
    pub(crate) reported_tcb: SnpTcb,
    pub(crate) chip_id: [u8; 64],
    pub(crate) committed_tcb: SnpTcb,
    pub(crate) launch_tcb: SnpTcb,
    signature: Signature,
}

/// What a report claims about the chip that produced it, read before anything
/// has been verified.
///
/// This exists so a caller can go and fetch the certificate that would settle
/// the claim. Nothing here is trusted: an attacker can put any chip id in an
/// unsigned report, and all that buys them is a certificate whose key cannot
/// sign what they sent. The claim is settled by
/// [`verify_sev_snp_attestation`](crate::verify_sev_snp_attestation), never by
/// this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimedOrigin {
    pub chip_id: [u8; 64],
    pub reported_tcb: SnpTcb,
}

/// Reads the chip and TCB a report names, checking only that the bytes are the
/// right shape to be a report at all.
pub fn claimed_origin(raw: &[u8]) -> Result<ClaimedOrigin, VerificationError> {
    if raw.len() != REPORT_LEN {
        return Err(VerificationError::SnpReportWrongSize);
    }
    if !REPORT_VERSIONS.contains(&u32_at(raw, OFF_VERSION)) {
        return Err(VerificationError::SnpReportVersionUnsupported);
    }
    // Everything a report must be before it is worth going to AMD about. A
    // fetch costs a request against a service that rate limits, so a report
    // that cannot verify under any certificate is refused before one is spent.
    if u32_at(raw, OFF_SIGNATURE_ALGO) != SIGNATURE_ALGO_ECDSA_P384_SHA384 {
        return Err(VerificationError::SnpReportSignatureAlgorithm);
    }
    if (u32_at(raw, OFF_FLAGS) >> 2) & 0b111 != SIGNING_KEY_VCEK {
        return Err(VerificationError::SnpReportNotVcekSigned);
    }
    Ok(ClaimedOrigin {
        chip_id: field(raw, OFF_CHIP_ID),
        reported_tcb: ProductLine::Genoa.decode_tcb(u64_at(raw, OFF_REPORTED_TCB)),
    })
}

pub(crate) fn parse_report(raw: &[u8], line: ProductLine) -> Result<Report, VerificationError> {
    if raw.len() != REPORT_LEN {
        return Err(VerificationError::SnpReportWrongSize);
    }
    if !REPORT_VERSIONS.contains(&u32_at(raw, OFF_VERSION)) {
        return Err(VerificationError::SnpReportVersionUnsupported);
    }
    if u32_at(raw, OFF_SIGNATURE_ALGO) != SIGNATURE_ALGO_ECDSA_P384_SHA384 {
        return Err(VerificationError::SnpReportSignatureAlgorithm);
    }
    if (u32_at(raw, OFF_FLAGS) >> 2) & 0b111 != SIGNING_KEY_VCEK {
        return Err(VerificationError::SnpReportNotVcekSigned);
    }

    Ok(Report {
        policy: GuestPolicy(u64_at(raw, OFF_POLICY)),
        vmpl: u32_at(raw, OFF_VMPL),
        current_tcb: line.decode_tcb(u64_at(raw, OFF_CURRENT_TCB)),
        report_data: field(raw, OFF_REPORT_DATA),
        measurement: field(raw, OFF_MEASUREMENT),
        host_data: field(raw, OFF_HOST_DATA),
        reported_tcb: line.decode_tcb(u64_at(raw, OFF_REPORTED_TCB)),
        chip_id: field(raw, OFF_CHIP_ID),
        committed_tcb: line.decode_tcb(u64_at(raw, OFF_COMMITTED_TCB)),
        launch_tcb: line.decode_tcb(u64_at(raw, OFF_LAUNCH_TCB)),
        signature: Signature::from_scalars(
            scalar(raw, OFF_SIGNATURE_R)?,
            scalar(raw, OFF_SIGNATURE_S)?,
        )
        .map_err(|_| VerificationError::SnpReportSignatureInvalid)?,
    })
}

/// The VCEK key is the leaf of the chain that was just anchored at the AMD
/// root, so a report that verifies here was produced by the PSP of the chip
/// that certificate names.
pub(crate) fn verify_report_signature(
    raw: &[u8],
    report: &Report,
    vcek_key: &VerifyingKey,
) -> Result<(), VerificationError> {
    vcek_key
        .verify(&raw[..SIGNED_LEN], &report.signature)
        .map_err(|_| VerificationError::SnpReportSignatureInvalid)
}

/// The report carries r and s little-endian in 72-byte fields. Anything set
/// above the low 48 bytes is not a P-384 scalar, and silently dropping it would
/// let two encodings of one signature both verify.
fn scalar(raw: &[u8], offset: usize) -> Result<[u8; SCALAR_LEN], VerificationError> {
    let field = &raw[offset..offset + SIGNATURE_FIELD_LEN];
    if field[SCALAR_LEN..].iter().any(|byte| *byte != 0) {
        return Err(VerificationError::SnpSignatureFieldOutOfRange);
    }

    let mut big_endian = [0_u8; SCALAR_LEN];
    for (index, byte) in field[..SCALAR_LEN].iter().enumerate() {
        big_endian[SCALAR_LEN - 1 - index] = *byte;
    }
    Ok(big_endian)
}

fn field<const N: usize>(raw: &[u8], offset: usize) -> [u8; N] {
    let mut out = [0_u8; N];
    out.copy_from_slice(&raw[offset..offset + N]);
    out
}

fn u32_at(raw: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(field(raw, offset))
}

fn u64_at(raw: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(field(raw, offset))
}

/// Builds reports in the layout the parser expects, so the vectors exercise the
/// real decoder instead of a second copy of it kept in the tests.
#[cfg(any(test, feature = "test-vectors"))]
#[derive(Debug, Clone)]
pub struct SnpReportBuilder {
    raw: [u8; REPORT_LEN],
}

#[cfg(any(test, feature = "test-vectors"))]
impl SnpReportBuilder {
    /// Bit 17 of the policy is reserved and must be set. SMT is allowed on the
    /// Dallas platform; debug, migration and single socket are not set.
    pub fn genoa(report_data: [u8; 64], measurement: [u8; 48], chip_id: [u8; 64]) -> Self {
        let mut raw = [0_u8; REPORT_LEN];
        raw[OFF_VERSION..OFF_VERSION + 4].copy_from_slice(&REPORT_VERSION_EMITTED.to_le_bytes());
        raw[OFF_SIGNATURE_ALGO..OFF_SIGNATURE_ALGO + 4]
            .copy_from_slice(&SIGNATURE_ALGO_ECDSA_P384_SHA384.to_le_bytes());
        raw[OFF_POLICY..OFF_POLICY + 8].copy_from_slice(&((1_u64 << 17) | (1 << 16)).to_le_bytes());
        raw[OFF_REPORT_DATA..OFF_REPORT_DATA + 64].copy_from_slice(&report_data);
        raw[OFF_MEASUREMENT..OFF_MEASUREMENT + 48].copy_from_slice(&measurement);
        raw[OFF_CHIP_ID..OFF_CHIP_ID + 64].copy_from_slice(&chip_id);
        Self { raw }
    }

    pub fn version(mut self, version: u32) -> Self {
        self.raw[OFF_VERSION..OFF_VERSION + 4].copy_from_slice(&version.to_le_bytes());
        self
    }

    pub fn vmpl(mut self, vmpl: u32) -> Self {
        self.raw[OFF_VMPL..OFF_VMPL + 4].copy_from_slice(&vmpl.to_le_bytes());
        self
    }

    pub fn report_data(mut self, report_data: [u8; 64]) -> Self {
        self.raw[OFF_REPORT_DATA..OFF_REPORT_DATA + 64].copy_from_slice(&report_data);
        self
    }

    pub fn host_data(mut self, host_data: [u8; 32]) -> Self {
        self.raw[OFF_HOST_DATA..OFF_HOST_DATA + 32].copy_from_slice(&host_data);
        self
    }

    pub fn measurement(mut self, measurement: [u8; 48]) -> Self {
        self.raw[OFF_MEASUREMENT..OFF_MEASUREMENT + 48].copy_from_slice(&measurement);
        self
    }

    pub fn chip_id(mut self, chip_id: [u8; 64]) -> Self {
        self.raw[OFF_CHIP_ID..OFF_CHIP_ID + 64].copy_from_slice(&chip_id);
        self
    }

    pub fn policy_bit(mut self, bit: u32) -> Self {
        let policy = u64::from_le_bytes(field(&self.raw, OFF_POLICY)) | (1 << bit);
        self.raw[OFF_POLICY..OFF_POLICY + 8].copy_from_slice(&policy.to_le_bytes());
        self
    }

    /// Genoa packing: bootloader at byte 0, TEE at byte 1, SNP at byte 6 and
    /// microcode at byte 7.
    pub fn tcb(mut self, bootloader: u8, tee: u8, snp: u8, microcode: u8) -> Self {
        let packed = pack_tcb(bootloader, tee, snp, microcode);
        for offset in [
            OFF_CURRENT_TCB,
            OFF_REPORTED_TCB,
            OFF_COMMITTED_TCB,
            OFF_LAUNCH_TCB,
        ] {
            self.raw[offset..offset + 8].copy_from_slice(&packed.to_le_bytes());
        }
        self
    }

    pub fn launch_tcb(mut self, bootloader: u8, tee: u8, snp: u8, microcode: u8) -> Self {
        let packed = pack_tcb(bootloader, tee, snp, microcode);
        self.raw[OFF_LAUNCH_TCB..OFF_LAUNCH_TCB + 8].copy_from_slice(&packed.to_le_bytes());
        self
    }

    pub fn signed_with(&self, vcek_key: &p384::ecdsa::SigningKey) -> Vec<u8> {
        use p384::ecdsa::signature::Signer;

        let mut raw = self.raw;
        let signature: Signature = vcek_key.sign(&raw[..SIGNED_LEN]);
        for (offset, scalar) in [
            (OFF_SIGNATURE_R, signature.r().to_bytes()),
            (OFF_SIGNATURE_S, signature.s().to_bytes()),
        ] {
            for (index, byte) in scalar.iter().rev().enumerate() {
                raw[offset + index] = *byte;
            }
        }
        raw.to_vec()
    }

    /// Signs, then sets a byte in the high half of the r field where a P-384
    /// scalar can never reach.
    pub fn signed_with_oversized_r(&self, vcek_key: &p384::ecdsa::SigningKey) -> Vec<u8> {
        let mut raw = self.signed_with(vcek_key);
        raw[OFF_SIGNATURE_R + SCALAR_LEN] = 0x01;
        raw
    }
}

#[cfg(any(test, feature = "test-vectors"))]
fn pack_tcb(bootloader: u8, tee: u8, snp: u8, microcode: u8) -> u64 {
    u64::from(bootloader)
        | (u64::from(tee) << 8)
        | (u64::from(snp) << 48)
        | (u64::from(microcode) << 56)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> p384::ecdsa::SigningKey {
        p384::ecdsa::SigningKey::from_bytes(&[0x42; 48].into()).expect("scalar")
    }

    #[test]
    fn a_report_of_any_other_size_is_rejected() {
        for len in [0, REPORT_LEN - 1, REPORT_LEN + 1] {
            assert_eq!(
                parse_report(&vec![0_u8; len], ProductLine::Genoa).unwrap_err(),
                VerificationError::SnpReportWrongSize
            );
        }
    }

    #[test]
    fn the_genoa_tcb_packing_round_trips() {
        let raw = SnpReportBuilder::genoa([0x11; 64], [0x22; 48], [0x33; 64])
            .tcb(10, 0, 25, 84)
            .signed_with(&key());
        let report = parse_report(&raw, ProductLine::Genoa).expect("report");

        assert_eq!(
            report.reported_tcb,
            SnpTcb {
                bootloader: 10,
                tee: 0,
                snp: 25,
                microcode: 84
            }
        );
        assert_eq!(report.launch_tcb, report.reported_tcb);
        assert_eq!(report.committed_tcb, report.reported_tcb);
        assert_eq!(report.current_tcb, report.reported_tcb);
    }

    #[test]
    fn the_policy_bits_decode_where_the_abi_puts_them() {
        let raw = SnpReportBuilder::genoa([0; 64], [0; 48], [0; 64]).signed_with(&key());
        let report = parse_report(&raw, ProductLine::Genoa).expect("report");
        assert!(report.policy.smt());
        assert!(!report.policy.debug());
        assert!(!report.policy.migrate_ma());
        assert!(!report.policy.single_socket());

        let raw = SnpReportBuilder::genoa([0; 64], [0; 48], [0; 64])
            .policy_bit(18)
            .policy_bit(19)
            .policy_bit(20)
            .signed_with(&key());
        let report = parse_report(&raw, ProductLine::Genoa).expect("report");
        assert!(report.policy.debug());
        assert!(report.policy.migrate_ma());
        assert!(report.policy.single_socket());
    }

    #[test]
    fn a_signature_over_the_report_verifies_only_over_the_signed_region() {
        let signing_key = key();
        let raw =
            SnpReportBuilder::genoa([0x11; 64], [0x22; 48], [0x33; 64]).signed_with(&signing_key);
        let report = parse_report(&raw, ProductLine::Genoa).expect("report");
        let verifying_key = VerifyingKey::from(&signing_key);
        assert!(verify_report_signature(&raw, &report, &verifying_key).is_ok());

        let mut tampered = raw.clone();
        tampered[OFF_MEASUREMENT] ^= 0x01;
        let report = parse_report(&tampered, ProductLine::Genoa).expect("report");
        assert_eq!(
            verify_report_signature(&tampered, &report, &verifying_key).unwrap_err(),
            VerificationError::SnpReportSignatureInvalid
        );
    }

    #[test]
    fn an_unknown_product_line_has_no_tcb_encoding() {
        assert!(ProductLine::parse("turin").is_none());
        assert_eq!(ProductLine::parse("genoa"), Some(ProductLine::Genoa));
    }
}
