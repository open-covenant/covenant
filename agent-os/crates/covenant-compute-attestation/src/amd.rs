//! The AMD certificate chain behind an SEV-SNP report: VCEK, ASK, ARK.
//!
//! The NVIDIA walk in `chain.rs` is P-384 end to end. This one is not: the ARK
//! and the ASK are RSA-4096 signing with PSS over SHA-384, and only the VCEK's
//! own key is P-384. The extension OIDs and their encodings are from the AMD
//! Versioned Chip Endorsement Key (VCEK) Certificate and KDS Interface
//! Specification, publication 57230, under the 1.3.6.1.4.1.3704.1 arc.

use crate::vendor_types::SnpTcb;
use chrono::{DateTime, Utc};
use p384::ecdsa::VerifyingKey;
#[cfg(any(test, feature = "test-vectors"))]
use p384::elliptic_curve::subtle::Choice;
use p384::elliptic_curve::subtle::ConstantTimeEq;
use rsa::RsaPublicKey;
use rsa::pkcs1::DecodeRsaPublicKey;
use rsa::pss::{Signature as PssSignature, VerifyingKey as PssVerifyingKey};
use rsa::signature::Verifier as _;
use rsa::traits::PublicKeyParts as _;
use sha2::{Digest, Sha256, Sha384};
use x509_parser::asn1_rs::{Oid, oid};
use x509_parser::certificate::X509Certificate;
use x509_parser::oid_registry::{
    OID_KEY_TYPE_EC_PUBLIC_KEY, OID_NIST_EC_P384, OID_NIST_HASH_SHA384, OID_PKCS1_RSAENCRYPTION,
    OID_PKCS1_RSASSAPSS,
};
use x509_parser::prelude::*;
use x509_parser::signature_algorithm::SignatureAlgorithm;
use x509_parser::x509::AlgorithmIdentifier;

use crate::VerificationError;
use crate::policy::Policy;

/// The AMD root every VCEK must chain to, pinned by content rather than by
/// name so a certificate that merely calls itself an ARK gets nowhere.
///
/// The file in the tree is the ARK AMD publishes for this product line, taken
/// from the chain it serves at `/vcek/v1/Genoa/cert_chain`. It is self-signed,
/// names AMD, and its digest is recorded in `reference/snp-platform.json` and
/// asserted at load, so a swapped file panics rather than being trusted. This
/// note used to say the file was a locally generated placeholder and that no
/// lease could reach Attested; both stopped being true once the real root
/// landed, and a comment claiming a root is fake is one somebody acts on.
const PINNED_ARK: &[u8] = include_bytes!("../roots/amd-ark-genoa.der");

#[cfg(any(test, feature = "test-vectors"))]
const TEST_ARK: &[u8] = include_bytes!("../tests/fixtures/snp/test-ark.der");

/// VCEK, ASK, ARK. AMD's chain is exactly three certificates, and a walk that
/// accepted a fourth would be accepting a link AMD never publishes.
const CHAIN_LEN: usize = 3;

/// AMD signs with a 4096 bit modulus and a salt the width of the digest.
const RSA_KEY_BYTES: usize = 512;
const PSS_SALT_LEN: usize = 48;

const OID_MGF1: Oid<'static> = oid!(1.2.840.113549.1.1.8);
const OID_AMD_BL_SPL: Oid<'static> = oid!(1.3.6.1.4.1.3704.1.3.1);
const OID_AMD_TEE_SPL: Oid<'static> = oid!(1.3.6.1.4.1.3704.1.3.2);
/// .3.3, not .3.7. The arc runs blSPL, teeSPL, snpSPL, then spl_4 through spl_7
/// as reserved, and ucodeSPL last. .3.7 is one of the reserved slots and reads 0
/// on every real certificate, so pointing at it makes a genuine VCEK disagree
/// with its own report's TCB.
const OID_AMD_SNP_SPL: Oid<'static> = oid!(1.3.6.1.4.1.3704.1.3.3);
const OID_AMD_UCODE_SPL: Oid<'static> = oid!(1.3.6.1.4.1.3704.1.3.8);
const OID_AMD_HWID: Oid<'static> = oid!(1.3.6.1.4.1.3704.1.4);

pub(crate) struct VerifiedVcek {
    pub(crate) public_key: VerifyingKey,
    /// The chip the VCEK was issued to. The report has to name the same one, or
    /// this certificate belongs to another machine.
    pub(crate) hwid: [u8; 64],
    /// The TCB the VCEK was keyed against. The report has to claim the same
    /// one, or this certificate belongs to another patch level.
    pub(crate) tcb: SnpTcb,
}

/// Walks VCEK, ASK, ARK and returns what the VCEK says about the chip and the
/// TCB. Every failure is terminal: there is no partial result a caller could
/// mistake for a grant.
pub(crate) fn verify_vcek_chain(
    chain: &[Vec<u8>],
    now: DateTime<Utc>,
    policy: &Policy,
) -> Result<VerifiedVcek, VerificationError> {
    if chain.len() != CHAIN_LEN {
        return Err(VerificationError::SnpChainShape);
    }

    let mut parsed = Vec::with_capacity(CHAIN_LEN);
    for der in chain {
        let (trailing, certificate) =
            parse_x509_certificate(der).map_err(|_| VerificationError::SnpMalformedCertificate)?;
        if !trailing.is_empty() {
            return Err(VerificationError::SnpMalformedCertificate);
        }
        parsed.push(certificate);
    }

    let (vcek, ask, ark) = (&parsed[0], &parsed[1], &parsed[2]);

    if !is_pinned_ark(&chain[2], policy) {
        return Err(VerificationError::SnpUntrustedRoot);
    }

    let now = now.timestamp();
    for (index, certificate) in parsed.iter().enumerate() {
        require_pss_sha384(&certificate.signature_algorithm)?;
        // The algorithm the issuer signed under is inside the signed bytes; the
        // one a verifier reads is not. They have to agree or the outer value is
        // an unauthenticated hint.
        if certificate.signature_algorithm != certificate.tbs_certificate.signature {
            return Err(VerificationError::SnpChainAlgorithm);
        }
        if !policy.allows_expired_certificates() {
            let validity = certificate.validity();
            if now < validity.not_before.timestamp() || now > validity.not_after.timestamp() {
                return Err(VerificationError::SnpCertificateExpired);
            }
        }
        require_ca(certificate, index != 0)?;
    }

    if ark.issuer() != ark.subject() {
        return Err(VerificationError::SnpChainBroken);
    }
    let ark_key = rsa_public_key(ark.public_key())?;
    verify_pss(
        &ark_key,
        ark.tbs_certificate.as_ref(),
        &ark.signature_value.data,
    )?;

    if ask.issuer() != ark.subject() {
        return Err(VerificationError::SnpChainBroken);
    }
    verify_pss(
        &ark_key,
        ask.tbs_certificate.as_ref(),
        &ask.signature_value.data,
    )?;

    if vcek.issuer() != ask.subject() {
        return Err(VerificationError::SnpChainBroken);
    }
    let ask_key = rsa_public_key(ask.public_key())?;
    verify_pss(
        &ask_key,
        vcek.tbs_certificate.as_ref(),
        &vcek.signature_value.data,
    )?;

    Ok(VerifiedVcek {
        public_key: p384_public_key(vcek.public_key())?,
        hwid: hwid_extension(vcek)?,
        tcb: SnpTcb {
            bootloader: svn_extension(vcek, &OID_AMD_BL_SPL)?,
            tee: svn_extension(vcek, &OID_AMD_TEE_SPL)?,
            snp: svn_extension(vcek, &OID_AMD_SNP_SPL)?,
            microcode: svn_extension(vcek, &OID_AMD_UCODE_SPL)?,
        },
    })
}

/// The reference file records the digest of the root this build is compiled
/// against, so a swapped root file with an unchanged reference is caught at
/// load rather than trusted.
pub(crate) fn pinned_ark_sha256() -> [u8; 32] {
    Sha256::digest(PINNED_ARK).into()
}

/// The AMD root is the only anchor a service build can reach. The fixture root
/// additionally needs a policy no service constructs, so a build that somehow
/// carried the test feature still cannot be talked into trusting it.
fn is_pinned_ark(der: &[u8], _policy: &Policy) -> bool {
    let presented = Sha256::digest(der);
    let trusted = presented.ct_eq(&Sha256::digest(PINNED_ARK)[..]);

    #[cfg(any(test, feature = "test-vectors"))]
    let trusted = trusted
        | (presented.ct_eq(&Sha256::digest(TEST_ARK)[..])
            & Choice::from(u8::from(_policy.trusts_test_root())));

    bool::from(trusted)
}

/// PSS with SHA-384 and MGF1-SHA-384 is the only algorithm AMD signs these
/// certificates with. PKCS#1 v1.5 under the same key is a different scheme and
/// is rejected here rather than at the signature check.
fn require_pss_sha384(algorithm: &AlgorithmIdentifier<'_>) -> Result<(), VerificationError> {
    let SignatureAlgorithm::RSASSA_PSS(params) = SignatureAlgorithm::try_from(algorithm)
        .map_err(|_| VerificationError::SnpChainAlgorithm)?
    else {
        return Err(VerificationError::SnpChainAlgorithm);
    };

    if *params.hash_algorithm_oid() != OID_NIST_HASH_SHA384 {
        return Err(VerificationError::SnpChainAlgorithm);
    }
    let mask_gen = params
        .mask_gen_algorithm()
        .map_err(|_| VerificationError::SnpChainAlgorithm)?;
    if mask_gen.mgf != OID_MGF1 || mask_gen.hash != OID_NIST_HASH_SHA384 {
        return Err(VerificationError::SnpChainAlgorithm);
    }
    if params.salt_length() != PSS_SALT_LEN as u32 || params.trailer_field() != 1 {
        return Err(VerificationError::SnpChainAlgorithm);
    }
    Ok(())
}

fn verify_pss(
    issuer_key: &RsaPublicKey,
    message: &[u8],
    signature: &[u8],
) -> Result<(), VerificationError> {
    let signature =
        PssSignature::try_from(signature).map_err(|_| VerificationError::SnpChainBroken)?;
    PssVerifyingKey::<Sha384>::new_with_salt_len(issuer_key.clone(), PSS_SALT_LEN)
        .verify(message, &signature)
        .map_err(|_| VerificationError::SnpChainBroken)
}

/// A VCEK that claimed CA rights could mint sibling chip identities, so the end
/// of the chain has to be an end entity. An absent extension is an end entity
/// by RFC 5280, which is why only an explicit true is rejected there.
fn require_ca(certificate: &X509Certificate<'_>, ca: bool) -> Result<(), VerificationError> {
    let declared = certificate
        .basic_constraints()
        .map_err(|_| VerificationError::SnpMalformedCertificate)?
        .map(|extension| extension.value.ca);

    let acceptable = if ca {
        declared == Some(true)
    } else {
        declared != Some(true)
    };
    if acceptable {
        Ok(())
    } else {
        Err(VerificationError::SnpMalformedCertificate)
    }
}

/// AMD carries the key under rsaEncryption. The PKCS#1 body is identical under
/// id-RSASSA-PSS, which some issuers use to restrict the key to one scheme, so
/// both identifiers are read and neither widens what is accepted.
fn rsa_public_key(spki: &SubjectPublicKeyInfo<'_>) -> Result<RsaPublicKey, VerificationError> {
    if spki.algorithm.algorithm != OID_PKCS1_RSAENCRYPTION
        && spki.algorithm.algorithm != OID_PKCS1_RSASSAPSS
    {
        return Err(VerificationError::SnpChainAlgorithm);
    }

    let key = RsaPublicKey::from_pkcs1_der(&spki.subject_public_key.data)
        .map_err(|_| VerificationError::SnpMalformedCertificate)?;
    if key.size() != RSA_KEY_BYTES {
        return Err(VerificationError::SnpChainKeySize);
    }
    Ok(key)
}

fn p384_public_key(spki: &SubjectPublicKeyInfo<'_>) -> Result<VerifyingKey, VerificationError> {
    if spki.algorithm.algorithm != OID_KEY_TYPE_EC_PUBLIC_KEY {
        return Err(VerificationError::SnpChainAlgorithm);
    }
    let curve = spki
        .algorithm
        .parameters
        .as_ref()
        .and_then(|parameters| parameters.as_oid().ok())
        .ok_or(VerificationError::SnpMalformedCertificate)?;
    if curve != OID_NIST_EC_P384 {
        return Err(VerificationError::SnpChainAlgorithm);
    }

    VerifyingKey::from_sec1_bytes(&spki.subject_public_key.data)
        .map_err(|_| VerificationError::SnpMalformedCertificate)
}

/// Certificates AMD's KDS issues today carry the chip id as 64 bare bytes, with
/// no tag and no length. The specification describes an OCTET STRING, and some
/// tooling emits one, so both are read here. Requiring the wrapper rejects every
/// genuine VCEK, which is the more expensive way to be wrong.
fn hwid_extension(vcek: &X509Certificate<'_>) -> Result<[u8; 64], VerificationError> {
    let value = amd_extension(vcek, &OID_AMD_HWID)?;
    let body = match value.len() {
        64 => value,
        66 if value[0] == 0x04 && value[1] == 64 => &value[2..],
        _ => return Err(VerificationError::SnpVcekExtensionMalformed),
    };

    let mut hwid = [0_u8; 64];
    hwid.copy_from_slice(body);
    Ok(hwid)
}

/// AMD has shipped the SVN extensions as both DER INTEGER and OCTET STRING, so
/// both are read. Each carries one byte and anything wider is refused rather
/// than truncated into a lower security version.
fn svn_extension(vcek: &X509Certificate<'_>, oid: &Oid<'_>) -> Result<u8, VerificationError> {
    decode_svn(amd_extension(vcek, oid)?)
}

fn decode_svn(value: &[u8]) -> Result<u8, VerificationError> {
    if value.len() < 3 || usize::from(value[1]) != value.len() - 2 {
        return Err(VerificationError::SnpVcekExtensionMalformed);
    }

    let body = &value[2..];
    let significant = match value[0] {
        0x02 if body.len() > 1 && body[0] == 0 => &body[1..],
        0x02 | 0x04 => body,
        _ => return Err(VerificationError::SnpVcekExtensionMalformed),
    };
    match significant {
        [svn] => Ok(*svn),
        _ => Err(VerificationError::SnpVcekExtensionMalformed),
    }
}

/// A second extension under the same OID would let a signer present two chips
/// or two patch levels and leave the choice to the reader, so a repeat is
/// malformed rather than resolved.
fn amd_extension<'a>(
    certificate: &'a X509Certificate<'a>,
    oid: &Oid<'_>,
) -> Result<&'a [u8], VerificationError> {
    let mut found = None;
    for extension in certificate.extensions() {
        if extension.oid != *oid {
            continue;
        }
        if found.is_some() {
            return Err(VerificationError::SnpVcekExtensionMalformed);
        }
        found = Some(extension.value);
    }
    found.ok_or(VerificationError::SnpVcekExtensionMissing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_svn_extension_reads_both_encodings_amd_ships() {
        assert_eq!(decode_svn(&[0x02, 0x01, 0x19]), Ok(0x19));
        assert_eq!(decode_svn(&[0x04, 0x01, 0x19]), Ok(0x19));
        assert_eq!(decode_svn(&[0x02, 0x02, 0x00, 0xd4]), Ok(0xd4));
        assert_eq!(decode_svn(&[0x02, 0x01, 0x00]), Ok(0));
    }

    #[test]
    fn a_wider_svn_is_refused_rather_than_truncated() {
        for value in [
            [0x02, 0x02, 0x01, 0x19].as_slice(),
            [0x04, 0x02, 0x01, 0x19].as_slice(),
            [0x0c, 0x01, 0x19].as_slice(),
            [0x02, 0x01].as_slice(),
            [0x02, 0x02, 0x19].as_slice(),
        ] {
            assert_eq!(
                decode_svn(value),
                Err(VerificationError::SnpVcekExtensionMalformed)
            );
        }
    }
}
