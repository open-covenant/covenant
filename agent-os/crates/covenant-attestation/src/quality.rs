//! The quality attestation — the spec-gated release leg ("never pay for junk").
//!
//! A [`SpendGrantEscrow`] call holds a provider's payment in escrow until an
//! output clears a spec bar. The daemon judges the output, then signs this
//! secp256k1 EIP-712 [`QualityAttestation`]; the escrow releases the hold
//! (minus fee) only when `ecrecover` over the digest returns Covenant's
//! `attestor`. The verdict is not logged and reconciled after the fact — it is
//! the precondition the contract enforces before any money moves. A `passed =
//! false` verdict is the junk path: the same signature unwinds the hold back to
//! the grant immediately, without waiting out the call deadline.
//!
//! This is a sibling of [`crate::BondReceipt`] and shares its EIP-712 shape.
//! The one deliberate difference: a bond receipt omits `verifyingContract` so
//! it survives verifier redeploys, but a quality verdict *authorizes a payout
//! from one specific escrow*, so its domain binds `verifyingContract` — a
//! verdict signed for one deployment cannot release a hold on another, even at
//! the same chain id. `chainId` is bound for the same reason.
//!
//! The reference verifier is `agent-os/evm/contracts/SpendGrantEscrow.sol`
//! (`_verifyQuality` / `releaseCallAttested`); its `test_Golden_QualityDigest`
//! pins the same typehash, domain separator, and digest this module reproduces.

use covenant_identity::Secp256k1IssuerKey;
use k256::ecdsa::{RecoveryId, Signature as EcdsaSignature, VerifyingKey};
use sha3::{Digest, Keccak256};

const DOMAIN_NAME: &str = "Covenant SpendGrant";
const DOMAIN_VERSION: &str = "1";
const EIP712_DOMAIN_TYPE: &[u8] =
    b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)";
const QUALITY_TYPE: &[u8] =
    b"Quality(uint256 callId,bytes32 resultHash,bool passed,bytes32 specId,uint256 deadline)";

/// Every way a quality attestation fails to construct, sign, or verify. Each
/// variant names the specific check so a reviewer can tell a forged verdict
/// from a malformed one.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum QualityError {
    #[error("chain id must be non-zero")]
    ZeroChainId,
    #[error("verifying contract address is all-zero")]
    ZeroVerifyingContract,
    #[error("attestation deadline must be greater than zero")]
    ZeroDeadline,
    #[error("quality attestation expired: now {now} > deadline {deadline}")]
    Expired { now: u64, deadline: u64 },
    #[error("recovered signer is not the trusted attestor")]
    UntrustedSigner,
    #[error("malformed signature: {0}")]
    Signature(String),
}

/// A Covenant-signed verdict on one escrowed call's output — the release (or
/// junk-refund) precondition [`SpendGrantEscrow`] authenticates with one
/// `ecrecover`. Every field is committed into the EIP-712 digest.
///
/// Constructed as a literal; the fields are re-checked by
/// [`QualityAttestation::validate`], which [`QualityAttestation::sign`] runs
/// before signing so an ill-formed verdict is never signed.
#[derive(Debug, Clone)]
pub struct QualityAttestation {
    /// The chain id of the escrow deployment this verdict releases against.
    pub chain_id: u64,
    /// The deployed `SpendGrantEscrow` address, bound into the domain.
    pub verifying_contract: [u8; 20],
    /// The escrow call id this verdict settles.
    pub call_id: u128,
    /// The judged output's result hash, committed so the payout records the
    /// exact bytes the verdict covers.
    pub result_hash: [u8; 32],
    /// The verdict: `true` releases to the provider, `false` refunds the hold.
    pub passed: bool,
    /// The spec the output was judged against.
    pub spec_id: [u8; 32],
    /// Verdict expiry (unix seconds); the escrow rejects a stale attestation.
    pub deadline: u64,
}

impl QualityAttestation {
    /// Fail-closed structural checks, independent of any signature. These bound
    /// the *target* (a real chain, a real deployment, a real deadline); the
    /// semantic verdict (`passed`, which spec) is the daemon's to decide.
    pub fn validate(&self) -> Result<(), QualityError> {
        if self.chain_id == 0 {
            return Err(QualityError::ZeroChainId);
        }
        if self.verifying_contract == [0u8; 20] {
            return Err(QualityError::ZeroVerifyingContract);
        }
        if self.deadline == 0 {
            return Err(QualityError::ZeroDeadline);
        }
        Ok(())
    }

    /// `keccak256(abi.encode(typeHash, keccak(name), keccak(version), chainId, verifyingContract))`.
    pub fn domain_separator(&self) -> [u8; 32] {
        let mut buf = Vec::with_capacity(32 * 5);
        buf.extend_from_slice(&keccak256(EIP712_DOMAIN_TYPE));
        buf.extend_from_slice(&keccak256(DOMAIN_NAME.as_bytes()));
        buf.extend_from_slice(&keccak256(DOMAIN_VERSION.as_bytes()));
        buf.extend_from_slice(&uint256(u128::from(self.chain_id)));
        buf.extend_from_slice(&address_word(&self.verifying_contract));
        keccak256(&buf)
    }

    fn struct_hash(&self) -> [u8; 32] {
        let mut buf = Vec::with_capacity(32 * 6);
        buf.extend_from_slice(&keccak256(QUALITY_TYPE));
        buf.extend_from_slice(&uint256(self.call_id));
        buf.extend_from_slice(&self.result_hash);
        buf.extend_from_slice(&uint256(u128::from(self.passed)));
        buf.extend_from_slice(&self.spec_id);
        buf.extend_from_slice(&uint256(u128::from(self.deadline)));
        keccak256(&buf)
    }

    /// The EIP-712 signing digest: `keccak256(0x1901 ‖ domainSeparator ‖ structHash)`.
    pub fn digest(&self) -> [u8; 32] {
        let mut buf = Vec::with_capacity(2 + 32 + 32);
        buf.push(0x19);
        buf.push(0x01);
        buf.extend_from_slice(&self.domain_separator());
        buf.extend_from_slice(&self.struct_hash());
        keccak256(&buf)
    }

    /// Validate, then sign with Covenant's secp256k1 attestor key. Both
    /// verdicts sign: `passed = true` drives release, `passed = false` drives
    /// the junk refund — the escrow separates them, the signer does not.
    pub fn sign(
        &self,
        attestor: &Secp256k1IssuerKey,
    ) -> Result<SignedQualityAttestation, QualityError> {
        self.validate()?;
        let signature = attestor.sign_eip712_digest(&self.digest());
        Ok(SignedQualityAttestation {
            attestation: self.clone(),
            signature,
        })
    }
}

/// A [`QualityAttestation`] plus the attestor's 65-byte `r ‖ s ‖ v` signature —
/// the artifact `covenantd` submits to `releaseCallAttested` /
/// `refundCallAttested`.
#[derive(Debug, Clone)]
pub struct SignedQualityAttestation {
    attestation: QualityAttestation,
    signature: [u8; 65],
}

impl SignedQualityAttestation {
    pub fn attestation(&self) -> &QualityAttestation {
        &self.attestation
    }

    /// The 65-byte `r ‖ s ‖ v` signature, low-S normalized, `v ∈ {27, 28}`.
    pub fn signature(&self) -> &[u8; 65] {
        &self.signature
    }

    pub fn digest(&self) -> [u8; 32] {
        self.attestation.digest()
    }

    pub fn passed(&self) -> bool {
        self.attestation.passed
    }

    /// Recover the signer address from the signature over the digest.
    pub fn recover_signer(&self) -> Result<[u8; 20], QualityError> {
        recover_address(&self.attestation.digest(), &self.signature)
    }

    /// Authenticity + freshness, exactly as the escrow checks before it
    /// branches on `passed`: re-validate the target, reject once `now` passes
    /// the attestation `deadline` (inclusive upper bound, matching the
    /// contract's `block.timestamp > deadline`), and require the recovered
    /// signer to be the trusted attestor. The caller then reads [`passed`] to
    /// branch release vs refund. The escrow additionally gates *release* on the
    /// call's own deadline — on-chain state this off-chain check does not hold.
    ///
    /// [`passed`]: SignedQualityAttestation::passed
    pub fn verify(&self, trusted_attestor: &[u8; 20], now: u64) -> Result<(), QualityError> {
        self.attestation.validate()?;
        if now > self.attestation.deadline {
            return Err(QualityError::Expired {
                now,
                deadline: self.attestation.deadline,
            });
        }
        if &self.recover_signer()? != trusted_attestor {
            return Err(QualityError::UntrustedSigner);
        }
        Ok(())
    }

    /// The 128-byte input to the `ecrecover` precompile (address `0x01`):
    /// `digest ‖ v ‖ r ‖ s`, each a 32-byte word, `v` right-aligned. Calling
    /// the precompile with this input returns the same address as
    /// [`recover_signer`].
    ///
    /// [`recover_signer`]: SignedQualityAttestation::recover_signer
    pub fn ecrecover_precompile_calldata(&self) -> [u8; 128] {
        let mut calldata = [0u8; 128];
        calldata[..32].copy_from_slice(&self.attestation.digest());
        calldata[63] = self.signature[64];
        calldata[64..96].copy_from_slice(&self.signature[..32]);
        calldata[96..128].copy_from_slice(&self.signature[32..64]);
        calldata
    }
}

fn keccak256(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Keccak256::digest(bytes));
    out
}

/// A `uint256` ABI word from a `u128`: 32-byte big-endian, left-padded.
fn uint256(value: u128) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[16..].copy_from_slice(&value.to_be_bytes());
    word
}

/// An `address` ABI word: the 20-byte address right-aligned in 32 bytes.
fn address_word(address: &[u8; 20]) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[12..].copy_from_slice(address);
    word
}

fn recover_address(digest: &[u8; 32], signature: &[u8; 65]) -> Result<[u8; 20], QualityError> {
    let recovery = signature[64]
        .checked_sub(27)
        .filter(|&b| b <= 1)
        .and_then(RecoveryId::from_byte)
        .ok_or_else(|| QualityError::Signature(format!("bad recovery byte {}", signature[64])))?;
    let sig = EcdsaSignature::from_slice(&signature[..64])
        .map_err(|e| QualityError::Signature(e.to_string()))?;
    // Covenant only ever emits low-S (see `Secp256k1IssuerKey::sign_eip712_digest`),
    // so the malleable high-S twin is never a legitimate verdict. `ecrecover`
    // cannot reject it, so the off-chain path canonicalizes here.
    if sig.normalize_s().is_some() {
        return Err(QualityError::Signature(
            "non-canonical high-S signature".into(),
        ));
    }
    let key = VerifyingKey::recover_from_prehash(digest, &sig, recovery)
        .map_err(|e| QualityError::Signature(e.to_string()))?;
    Ok(eth_address(&key))
}

fn eth_address(vk: &VerifyingKey) -> [u8; 20] {
    let encoded = vk.to_encoded_point(false);
    let hash = keccak256(&encoded.as_bytes()[1..]);
    let mut address = [0u8; 20];
    address.copy_from_slice(&hash[12..]);
    address
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multibase::hex_encode;

    fn attestor() -> Secp256k1IssuerKey {
        Secp256k1IssuerKey::from_secret_bytes(&[9u8; 32]).unwrap()
    }

    fn sample() -> QualityAttestation {
        QualityAttestation {
            chain_id: 42_161,
            verifying_contract: [0x0C; 20],
            call_id: 7,
            result_hash: [0x11; 32],
            passed: true,
            spec_id: [0x22; 32],
            deadline: 1_800_000_000,
        }
    }

    #[test]
    fn sign_then_verify_round_trip() {
        let key = attestor();
        let signed = sample().sign(&key).unwrap();
        assert!(signed.verify(&key.address(), 1_750_000_000).is_ok());
        assert_eq!(signed.recover_signer().unwrap(), key.address());
        assert!(matches!(signed.signature()[64], 27 | 28));
        assert!(signed.passed());
    }

    #[test]
    fn deadline_is_enforced_inclusive() {
        let key = attestor();
        let signed = sample().sign(&key).unwrap();
        let addr = key.address();
        assert!(signed.verify(&addr, 1_800_000_000).is_ok());
        assert_eq!(
            signed.verify(&addr, 1_800_000_001),
            Err(QualityError::Expired {
                now: 1_800_000_001,
                deadline: 1_800_000_000
            })
        );
    }

    #[test]
    fn junk_verdict_signs_and_recovers() {
        // A `passed = false` verdict is a first-class artifact: it drives the
        // immediate junk refund, so it must sign and recover like any other.
        let key = attestor();
        let mut att = sample();
        att.passed = false;
        let signed = att.sign(&key).unwrap();
        assert!(!signed.passed());
        assert!(signed.verify(&key.address(), 1_750_000_000).is_ok());
        // The verdict bit is inside the digest: the pass/fail twins differ.
        assert_ne!(sample().digest(), att.digest());
    }

    #[test]
    fn wrong_attestor_is_rejected() {
        let signed = sample().sign(&attestor()).unwrap();
        let other = Secp256k1IssuerKey::from_secret_bytes(&[5u8; 32]).unwrap();
        assert_eq!(
            signed.verify(&other.address(), 1_750_000_000),
            Err(QualityError::UntrustedSigner)
        );
    }

    #[test]
    fn deployment_is_bound_into_the_signature() {
        // Re-present a verdict against a *different* escrow deployment by
        // swapping verifyingContract, keeping the signature. The domain
        // changes, so ecrecover no longer returns the attestor.
        let key = attestor();
        let signed = sample().sign(&key).unwrap();
        let mut forged = signed.clone();
        forged.attestation.verifying_contract = [0xEE; 20];
        assert_eq!(
            forged.verify(&key.address(), 1_750_000_000),
            Err(QualityError::UntrustedSigner)
        );
    }

    #[test]
    fn domain_binds_chain_and_contract() {
        let base = sample();
        let mut other_chain = sample();
        other_chain.chain_id = 8453;
        let mut other_contract = sample();
        other_contract.verifying_contract = [0x0D; 20];
        assert_ne!(base.domain_separator(), other_chain.domain_separator());
        assert_ne!(base.domain_separator(), other_contract.domain_separator());
        assert_ne!(base.digest(), other_chain.digest());
        assert_ne!(base.digest(), other_contract.digest());
    }

    #[test]
    fn zero_target_fields_are_refused() {
        let key = attestor();
        let mut zero_chain = sample();
        zero_chain.chain_id = 0;
        assert_eq!(zero_chain.validate(), Err(QualityError::ZeroChainId));
        assert_eq!(
            zero_chain.sign(&key).unwrap_err(),
            QualityError::ZeroChainId
        );

        let mut zero_contract = sample();
        zero_contract.verifying_contract = [0u8; 20];
        assert_eq!(
            zero_contract.validate(),
            Err(QualityError::ZeroVerifyingContract)
        );

        let mut zero_deadline = sample();
        zero_deadline.deadline = 0;
        assert_eq!(zero_deadline.validate(), Err(QualityError::ZeroDeadline));
    }

    #[test]
    fn signing_is_deterministic() {
        let key = attestor();
        let a = sample().sign(&key).unwrap();
        let b = sample().sign(&key).unwrap();
        assert_eq!(a.signature(), b.signature());
        assert_eq!(a.digest(), b.digest());
    }

    #[test]
    fn ecrecover_precompile_calldata_matches_local_recovery() {
        let key = attestor();
        let signed = sample().sign(&key).unwrap();
        let calldata = signed.ecrecover_precompile_calldata();
        assert_eq!(&calldata[..32], &signed.digest());
        assert_eq!(calldata[63], signed.signature()[64]);
        assert_eq!(&calldata[64..96], &signed.signature()[..32]);
        assert_eq!(&calldata[96..128], &signed.signature()[32..64]);

        let mut sig = [0u8; 65];
        sig[..32].copy_from_slice(&calldata[64..96]);
        sig[32..64].copy_from_slice(&calldata[96..128]);
        sig[64] = calldata[63];
        let digest: [u8; 32] = calldata[..32].try_into().unwrap();
        assert_eq!(recover_address(&digest, &sig).unwrap(), key.address());
    }

    // secp256k1 group order N, big-endian, for building the high-S twin.
    const SECP256K1_N: [u8; 32] = [
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xfe, 0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36,
        0x41, 0x41,
    ];

    fn be_sub(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
        let mut out = [0u8; 32];
        let mut borrow: i16 = 0;
        for i in (0..32).rev() {
            let mut d = a[i] as i16 - b[i] as i16 - borrow;
            if d < 0 {
                d += 256;
                borrow = 1;
            } else {
                borrow = 0;
            }
            out[i] = d as u8;
        }
        out
    }

    #[test]
    fn high_s_twin_is_rejected() {
        let key = attestor();
        let mut signed = sample().sign(&key).unwrap();
        let mut s = [0u8; 32];
        s.copy_from_slice(&signed.signature[32..64]);
        signed.signature[32..64].copy_from_slice(&be_sub(&SECP256K1_N, &s));
        signed.signature[64] = if signed.signature[64] == 27 { 28 } else { 27 };
        assert_eq!(
            signed.verify(&key.address(), 1_750_000_000),
            Err(QualityError::Signature(
                "non-canonical high-S signature".into()
            ))
        );
    }

    // Frozen cross-language vector. `SpendGrantEscrow.t.sol::test_Golden_QualityDigest`
    // pins the same typehash, domain separator, and digest from these exact
    // inputs; if either side drifts, one of these literals stops matching.
    #[test]
    fn eip712_encoding_is_pinned() {
        assert_eq!(
            hex_encode(&keccak256(EIP712_DOMAIN_TYPE)),
            "8b73c3c69bb8fe3d512ecc4cf759cc79239f7b179b0ffacaa9a75d522b39400f"
        );
        assert_eq!(
            hex_encode(&keccak256(QUALITY_TYPE)),
            "0cf929082ca7b2e597467a7baa8bdad7b2f11a50345061c502617546ee02c5f7"
        );

        let mut c0de = [0u8; 20];
        c0de[18] = 0xc0;
        c0de[19] = 0xde;
        let golden = QualityAttestation {
            chain_id: 42_161,
            verifying_contract: c0de,
            call_id: 42,
            result_hash: [0x11; 32],
            passed: true,
            spec_id: [0x22; 32],
            deadline: 1_700_003_600,
        };
        assert_eq!(
            hex_encode(&golden.domain_separator()),
            "dfca76f14164933450f648cbe36072095dfe8b44f3dec7b405fb048035d7008b"
        );
        assert_eq!(
            hex_encode(&golden.struct_hash()),
            "39fd9b7405f9e77c3bd56af5e08184f94c06e73c3947b3355d77f76a53993899"
        );
        assert_eq!(
            hex_encode(&golden.digest()),
            "8dcd9e33febda1edb51bfa3a8aa86fcc7a50d330f1cd9fd50cace7ceb37ece45"
        );
    }
}
