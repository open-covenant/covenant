use chrono::{DateTime, Utc};
use p384::ecdsa::{Signature, VerifyingKey, signature::Verifier};
#[cfg(any(test, feature = "test-vectors"))]
use p384::elliptic_curve::subtle::Choice;
use p384::elliptic_curve::subtle::ConstantTimeEq;
use sha2::{Digest, Sha256};
use x509_parser::certificate::X509Certificate;
use x509_parser::oid_registry::{
    OID_KEY_TYPE_EC_PUBLIC_KEY, OID_NIST_EC_P384, OID_SIG_ECDSA_WITH_SHA384,
};
use x509_parser::prelude::*;

use crate::VerificationError;
use crate::policy::Policy;

/// The vendor root every device certificate must chain to. It is pinned by
/// content, not by name, so a certificate that merely calls itself NVIDIA gets
/// nowhere.
///
/// The file currently in the tree is a placeholder generated in this repository
/// with its private key discarded; until it is replaced with the DER published
/// by NVIDIA, no real device report can anchor and no node can be granted
/// Isolated on GPU evidence.
const PINNED_ROOT: &[u8] = include_bytes!("../roots/nvidia-device-identity-root.der");

#[cfg(any(test, feature = "test-vectors"))]
const TEST_ROOT: &[u8] = include_bytes!("../tests/fixtures/test-root.der");

pub(crate) struct VerifiedChain {
    pub(crate) leaf_public_key: VerifyingKey,
    pub(crate) leaf_common_name: String,
}

/// Walks the chain leaf first, root last, and returns the leaf key the device
/// report has to be signed under. Every failure is terminal: there is no
/// partial result a caller could mistake for a grant.
pub(crate) fn verify_chain(
    chain: &[Vec<u8>],
    max_certificates: usize,
    now: DateTime<Utc>,
    policy: &Policy,
) -> Result<VerifiedChain, VerificationError> {
    if chain.is_empty() {
        return Err(VerificationError::MalformedEvidence);
    }
    if chain.len() > max_certificates {
        return Err(VerificationError::ChainTooLong);
    }

    let mut parsed = Vec::with_capacity(chain.len());
    for der in chain {
        let (trailing, certificate) =
            parse_x509_certificate(der).map_err(|_| VerificationError::MalformedCertificate)?;
        if !trailing.is_empty() {
            return Err(VerificationError::MalformedCertificate);
        }
        parsed.push(certificate);
    }

    let root = chain.last().expect("chain emptiness is checked above");
    if !is_pinned_root(root, policy) {
        return Err(VerificationError::UntrustedRoot);
    }

    let now = now.timestamp();
    for (index, certificate) in parsed.iter().enumerate() {
        if certificate.signature_algorithm.algorithm != OID_SIG_ECDSA_WITH_SHA384 {
            return Err(VerificationError::WrongSignatureAlgorithm);
        }
        if !policy.allows_expired_certificates() {
            let validity = certificate.validity();
            if now < validity.not_before.timestamp() || now > validity.not_after.timestamp() {
                return Err(VerificationError::CertificateExpired);
            }
        }
        check_basic_constraints(certificate, index == 0)?;
    }

    for pair in parsed.windows(2) {
        let (subject, issuer) = (&pair[0], &pair[1]);
        if subject.issuer() != issuer.subject() {
            return Err(VerificationError::BrokenChain);
        }
        let issuer_key = p384_public_key(issuer.public_key())?;
        let signature = Signature::from_der(&subject.signature_value.data)
            .map_err(|_| VerificationError::BrokenChain)?;
        issuer_key
            .verify(subject.tbs_certificate.as_ref(), &signature)
            .map_err(|_| VerificationError::BrokenChain)?;
    }

    let leaf = &parsed[0];
    let leaf_common_name = leaf
        .subject()
        .iter_common_name()
        .next()
        .and_then(|name| name.as_str().ok())
        .ok_or(VerificationError::MalformedCertificate)?
        .to_string();

    Ok(VerifiedChain {
        leaf_public_key: p384_public_key(leaf.public_key())?,
        leaf_common_name,
    })
}

/// The vendor root is the only anchor a service build can reach. The fixture
/// root additionally needs a policy no service constructs, so a build that
/// somehow carried the test feature still cannot be talked into trusting it.
fn is_pinned_root(der: &[u8], _policy: &Policy) -> bool {
    let presented = Sha256::digest(der);
    let trusted = presented.ct_eq(&Sha256::digest(PINNED_ROOT)[..]);

    #[cfg(any(test, feature = "test-vectors"))]
    let trusted = trusted
        | (presented.ct_eq(&Sha256::digest(TEST_ROOT)[..])
            & Choice::from(u8::from(_policy.trusts_test_root())));

    bool::from(trusted)
}

/// A leaf that claims CA rights could mint sibling device identities, so the
/// end of the chain has to be an end entity. An absent extension is an end
/// entity by RFC 5280, which is why only an explicit true is rejected here.
fn check_basic_constraints(
    certificate: &X509Certificate<'_>,
    is_leaf: bool,
) -> Result<(), VerificationError> {
    let ca = certificate
        .basic_constraints()
        .map_err(|_| VerificationError::MalformedCertificate)?
        .map(|extension| extension.value.ca);

    if is_leaf {
        if ca == Some(true) {
            return Err(VerificationError::MalformedCertificate);
        }
    } else if ca != Some(true) {
        return Err(VerificationError::MalformedCertificate);
    }

    Ok(())
}

fn p384_public_key(spki: &SubjectPublicKeyInfo<'_>) -> Result<VerifyingKey, VerificationError> {
    if spki.algorithm.algorithm != OID_KEY_TYPE_EC_PUBLIC_KEY {
        return Err(VerificationError::WrongSignatureAlgorithm);
    }
    let curve = spki
        .algorithm
        .parameters
        .as_ref()
        .and_then(|parameters| parameters.as_oid().ok())
        .ok_or(VerificationError::MalformedCertificate)?;
    if curve != OID_NIST_EC_P384 {
        return Err(VerificationError::WrongSignatureAlgorithm);
    }

    VerifyingKey::from_sec1_bytes(&spki.subject_public_key.data)
        .map_err(|_| VerificationError::MalformedCertificate)
}
