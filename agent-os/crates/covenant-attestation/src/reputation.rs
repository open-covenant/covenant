//! The reputation attestation — Covenant's audit-derived score, made legible
//! onchain without a bridge.
//!
//! An agent's reputation is not a claim; it is a function of its own
//! tamper-evident audit chain, computed on Solana (see
//! `covenant-audit::reputation`): of the governed actions it attempted, the
//! fraction that stayed within its authority. This module carries that number
//! as a secp256k1 EIP-712 [`ReputationAttestation`] so any EVM contract — the
//! reference [`CovenantReputationRegistry`] on Robinhood Chain, or a consumer
//! on Base — can read `score`/`scoreDecimals` for a subject with one
//! `ecrecover`, and follow `solanaAttestation` back to the canonical record.
//!
//! This is a sibling of [`crate::QualityAttestation`] and shares its EIP-712
//! shape, with one deliberate difference in the domain. A quality verdict
//! *authorizes a payout from one specific escrow*, so its domain binds
//! `chainId` and `verifyingContract` — it must not release a hold anywhere
//! else. A reputation score *states a fact about an agent*, true on every
//! chain at once, so its domain binds neither: it uses a constant `salt` (as
//! [`crate::eip712`] does for the audit-root attestation). One issuer
//! signature is therefore portable — the same bytes verify on 4663, on Base,
//! and off-chain — because reputation is Solana-canonical and not a
//! chain-specific transaction.
//!
//! The reference verifier is `agent-os/evm/contracts/CovenantReputationRegistry.sol`;
//! its `test_Golden_ReputationDigest` pins the same typehash, domain separator,
//! and digest this module reproduces, and its live counterpart
//! `tests/live_rh_reputation_registry.rs` drives the deployed 4663 registry's
//! own `view` methods against these vectors.

use covenant_identity::Secp256k1IssuerKey;
use k256::ecdsa::{RecoveryId, Signature as EcdsaSignature, VerifyingKey};
use sha3::{Digest, Keccak256};

const DOMAIN_NAME: &str = "Covenant Reputation";
const DOMAIN_VERSION: &str = "1";
const DOMAIN_SALT_PREIMAGE: &[u8] = b"covenant/reputation/v1";
const EIP712_DOMAIN_TYPE: &[u8] = b"EIP712Domain(string name,string version,bytes32 salt)";
const REPUTATION_TYPE: &[u8] = b"Reputation(bytes32 subject,uint32 score,uint8 scoreDecimals,uint64 validUntil,string sourceChain,bytes32 solanaAttestation)";

/// The largest `scoreDecimals` the score bound is computed at; caps
/// `10^decimals` well inside `u128`. The canonical audit score uses 4.
pub const MAX_SCORE_DECIMALS: u8 = 18;

/// The canonical source chain for a Covenant reputation: the audit chain is
/// anchored on Solana, so a score's authority traces there regardless of which
/// EVM chain reads it.
pub const SOURCE_CHAIN_SOLANA: &str = "solana";

/// Every way a reputation attestation fails to construct, sign, or verify.
/// Each variant names the specific check so a reviewer can tell a forged score
/// from a malformed one.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReputationError {
    #[error("subject identity is all-zero")]
    ZeroSubject,
    #[error("validUntil must be greater than zero")]
    ZeroValidUntil,
    #[error("solana attestation pointer is all-zero")]
    ZeroSolanaAttestation,
    #[error("source chain must not be empty")]
    EmptySourceChain,
    #[error("scoreDecimals {decimals} exceeds the maximum {max}")]
    ScoreDecimalsTooLarge { decimals: u8, max: u8 },
    #[error("score {score} exceeds full scale {max} for {decimals} decimals")]
    ScoreOutOfRange { score: u32, max: u128, decimals: u8 },
    #[error("reputation attestation expired: now {now} > validUntil {valid_until}")]
    Expired { now: u64, valid_until: u64 },
    #[error("recovered signer is not the trusted attestor")]
    UntrustedSigner,
    #[error("malformed signature: {0}")]
    Signature(String),
}

/// A Covenant-signed reputation score for one agent — the legible fact a
/// [`CovenantReputationRegistry`] authenticates with one `ecrecover` and can
/// post onchain. Every field is committed into the EIP-712 digest.
///
/// Fields mirror the Base EAS reputation schema (`uint32 score`,
/// `uint8 score_decimals`, `uint64 expiry`, `string source_chain`,
/// `bytes32 solana_attestation_pda`) plus the `subject` the EAS envelope
/// carried as its recipient, so the same statement reads identically across
/// Covenant's settlement chains.
///
/// Constructed as a literal; the fields are re-checked by
/// [`ReputationAttestation::validate`], which [`ReputationAttestation::sign`]
/// runs before signing so an ill-formed score is never signed.
#[derive(Debug, Clone)]
pub struct ReputationAttestation {
    /// The agent's Solana identity (ed25519/PDA) — the subject the score is
    /// about, the same 32-byte binding [`crate::BondReceipt`] uses.
    pub subject: [u8; 32],
    /// The compliance score scaled to `score_decimals`; a ratio in `[0, 1]`,
    /// so `score <= 10^score_decimals`.
    pub score: u32,
    /// The scale `score` is expressed in (the audit chain uses 4).
    pub score_decimals: u8,
    /// Score expiry (unix seconds); a verifier rejects a stale attestation,
    /// which is what forces the daemon to re-attest as the audit chain grows.
    pub valid_until: u64,
    /// The chain the score's authority traces to — [`SOURCE_CHAIN_SOLANA`].
    pub source_chain: String,
    /// The Solana attestation the score was derived from, so a consumer can
    /// follow it back to the canonical record behind the number.
    pub solana_attestation: [u8; 32],
}

impl ReputationAttestation {
    /// Fail-closed structural checks, independent of any signature: a real
    /// subject, a real expiry, a resolvable canonical source, and a score that
    /// is a genuine `[0, 1]` ratio at its stated scale. The score's *meaning*
    /// (which audit history produced it) is the daemon's to decide.
    pub fn validate(&self) -> Result<(), ReputationError> {
        if self.subject == [0u8; 32] {
            return Err(ReputationError::ZeroSubject);
        }
        if self.valid_until == 0 {
            return Err(ReputationError::ZeroValidUntil);
        }
        if self.solana_attestation == [0u8; 32] {
            return Err(ReputationError::ZeroSolanaAttestation);
        }
        if self.source_chain.is_empty() {
            return Err(ReputationError::EmptySourceChain);
        }
        if self.score_decimals > MAX_SCORE_DECIMALS {
            return Err(ReputationError::ScoreDecimalsTooLarge {
                decimals: self.score_decimals,
                max: MAX_SCORE_DECIMALS,
            });
        }
        let full_scale = 10u128.pow(u32::from(self.score_decimals));
        if u128::from(self.score) > full_scale {
            return Err(ReputationError::ScoreOutOfRange {
                score: self.score,
                max: full_scale,
                decimals: self.score_decimals,
            });
        }
        Ok(())
    }

    /// `keccak256(abi.encode(typeHash, keccak(name), keccak(version), salt))`.
    ///
    /// A constant — no chain id, no verifying contract — so a signature over a
    /// digest built on it is portable across every chain. This is the whole
    /// distinction from [`QualityAttestation::domain_separator`].
    ///
    /// [`QualityAttestation::domain_separator`]: crate::QualityAttestation::domain_separator
    pub fn domain_separator() -> [u8; 32] {
        let mut buf = Vec::with_capacity(32 * 4);
        buf.extend_from_slice(&keccak256(EIP712_DOMAIN_TYPE));
        buf.extend_from_slice(&keccak256(DOMAIN_NAME.as_bytes()));
        buf.extend_from_slice(&keccak256(DOMAIN_VERSION.as_bytes()));
        buf.extend_from_slice(&salt());
        keccak256(&buf)
    }

    fn struct_hash(&self) -> [u8; 32] {
        let mut buf = Vec::with_capacity(32 * 7);
        buf.extend_from_slice(&keccak256(REPUTATION_TYPE));
        buf.extend_from_slice(&self.subject);
        buf.extend_from_slice(&uint256(u128::from(self.score)));
        buf.extend_from_slice(&uint256(u128::from(self.score_decimals)));
        buf.extend_from_slice(&uint256(u128::from(self.valid_until)));
        buf.extend_from_slice(&keccak256(self.source_chain.as_bytes()));
        buf.extend_from_slice(&self.solana_attestation);
        keccak256(&buf)
    }

    /// The EIP-712 signing digest: `keccak256(0x1901 ‖ domainSeparator ‖ structHash)`.
    pub fn digest(&self) -> [u8; 32] {
        let mut buf = Vec::with_capacity(2 + 32 + 32);
        buf.push(0x19);
        buf.push(0x01);
        buf.extend_from_slice(&Self::domain_separator());
        buf.extend_from_slice(&self.struct_hash());
        keccak256(&buf)
    }

    /// Validate, then sign with Covenant's secp256k1 attestor key.
    pub fn sign(
        &self,
        attestor: &Secp256k1IssuerKey,
    ) -> Result<SignedReputationAttestation, ReputationError> {
        self.validate()?;
        let signature = attestor.sign_eip712_digest(&self.digest());
        Ok(SignedReputationAttestation {
            attestation: self.clone(),
            signature,
        })
    }
}

/// A [`ReputationAttestation`] plus the attestor's 65-byte `r ‖ s ‖ v`
/// signature — the artifact `covenantd` submits to the registry's `verify` or
/// `postReputation`.
#[derive(Debug, Clone)]
pub struct SignedReputationAttestation {
    attestation: ReputationAttestation,
    signature: [u8; 65],
}

impl SignedReputationAttestation {
    pub fn attestation(&self) -> &ReputationAttestation {
        &self.attestation
    }

    /// The 65-byte `r ‖ s ‖ v` signature, low-S normalized, `v ∈ {27, 28}`.
    pub fn signature(&self) -> &[u8; 65] {
        &self.signature
    }

    pub fn digest(&self) -> [u8; 32] {
        self.attestation.digest()
    }

    /// Recover the signer address from the signature over the digest.
    pub fn recover_signer(&self) -> Result<[u8; 20], ReputationError> {
        recover_address(&self.attestation.digest(), &self.signature)
    }

    /// Authenticity + freshness, exactly as the registry checks: re-validate
    /// the score, reject once `now` passes `validUntil` (inclusive upper
    /// bound, matching the contract's `block.timestamp > validUntil`), and
    /// require the recovered signer to be the trusted attestor.
    pub fn verify(&self, trusted_attestor: &[u8; 20], now: u64) -> Result<(), ReputationError> {
        self.attestation.validate()?;
        if now > self.attestation.valid_until {
            return Err(ReputationError::Expired {
                now,
                valid_until: self.attestation.valid_until,
            });
        }
        if &self.recover_signer()? != trusted_attestor {
            return Err(ReputationError::UntrustedSigner);
        }
        Ok(())
    }

    /// The 128-byte input to the `ecrecover` precompile (address `0x01`):
    /// `digest ‖ v ‖ r ‖ s`, each a 32-byte word, `v` right-aligned. Calling
    /// the precompile with this input returns the same address as
    /// [`recover_signer`].
    ///
    /// [`recover_signer`]: SignedReputationAttestation::recover_signer
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

fn salt() -> [u8; 32] {
    keccak256(DOMAIN_SALT_PREIMAGE)
}

/// A `uint256` ABI word from a `u128`: 32-byte big-endian, left-padded.
fn uint256(value: u128) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[16..].copy_from_slice(&value.to_be_bytes());
    word
}

fn recover_address(digest: &[u8; 32], signature: &[u8; 65]) -> Result<[u8; 20], ReputationError> {
    let recovery = signature[64]
        .checked_sub(27)
        .filter(|&b| b <= 1)
        .and_then(RecoveryId::from_byte)
        .ok_or_else(|| {
            ReputationError::Signature(format!("bad recovery byte {}", signature[64]))
        })?;
    let sig = EcdsaSignature::from_slice(&signature[..64])
        .map_err(|e| ReputationError::Signature(e.to_string()))?;
    // Covenant only ever emits low-S (see `Secp256k1IssuerKey::sign_eip712_digest`),
    // so the malleable high-S twin is never a legitimate score.
    if sig.normalize_s().is_some() {
        return Err(ReputationError::Signature(
            "non-canonical high-S signature".into(),
        ));
    }
    let key = VerifyingKey::recover_from_prehash(digest, &sig, recovery)
        .map_err(|e| ReputationError::Signature(e.to_string()))?;
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

    fn sample() -> ReputationAttestation {
        ReputationAttestation {
            subject: [0xAB; 32],
            score: 9_500,
            score_decimals: 4,
            valid_until: 1_800_000_000,
            source_chain: SOURCE_CHAIN_SOLANA.to_string(),
            solana_attestation: [0xCD; 32],
        }
    }

    #[test]
    fn sign_then_verify_round_trip() {
        let key = attestor();
        let signed = sample().sign(&key).unwrap();
        assert!(signed.verify(&key.address(), 1_750_000_000).is_ok());
        assert_eq!(signed.recover_signer().unwrap(), key.address());
        assert!(matches!(signed.signature()[64], 27 | 28));
    }

    #[test]
    fn valid_until_is_enforced_inclusive() {
        let key = attestor();
        let signed = sample().sign(&key).unwrap();
        let addr = key.address();
        assert!(signed.verify(&addr, 1_800_000_000).is_ok());
        assert_eq!(
            signed.verify(&addr, 1_800_000_001),
            Err(ReputationError::Expired {
                now: 1_800_000_001,
                valid_until: 1_800_000_000
            })
        );
    }

    #[test]
    fn wrong_attestor_is_rejected() {
        let signed = sample().sign(&attestor()).unwrap();
        let other = Secp256k1IssuerKey::from_secret_bytes(&[5u8; 32]).unwrap();
        assert_eq!(
            signed.verify(&other.address(), 1_750_000_000),
            Err(ReputationError::UntrustedSigner)
        );
    }

    #[test]
    fn domain_is_chain_agnostic_and_constant() {
        // The distinction from quality.rs: the domain has no chain id and no
        // verifying contract, so it is one fixed value — the reason a single
        // signature is portable across chains. Two scores about different
        // subjects share the exact same domain; only their struct hashes (and
        // thus digests) differ.
        let a = sample();
        let mut b = sample();
        b.subject = [0x11; 32];
        b.score = 1;
        assert_eq!(
            ReputationAttestation::domain_separator(),
            ReputationAttestation::domain_separator()
        );
        assert_ne!(a.digest(), b.digest());
    }

    #[test]
    fn subject_is_bound_into_the_signature() {
        // Re-present a score against a different subject by swapping it and
        // keeping the signature. The struct hash changes, so ecrecover no
        // longer returns the attestor — a score cannot be lifted onto another
        // agent.
        let key = attestor();
        let signed = sample().sign(&key).unwrap();
        let mut forged = signed.clone();
        forged.attestation.subject = [0xEE; 32];
        assert_eq!(
            forged.verify(&key.address(), 1_750_000_000),
            Err(ReputationError::UntrustedSigner)
        );
    }

    #[test]
    fn score_and_source_are_bound_into_the_signature() {
        let key = attestor();
        let signed = sample().sign(&key).unwrap();

        let mut inflated = signed.clone();
        inflated.attestation.score = 10_000;
        assert_eq!(
            inflated.verify(&key.address(), 1_750_000_000),
            Err(ReputationError::UntrustedSigner)
        );

        let mut restated = signed.clone();
        restated.attestation.source_chain = "base".into();
        assert_eq!(
            restated.verify(&key.address(), 1_750_000_000),
            Err(ReputationError::UntrustedSigner)
        );
    }

    #[test]
    fn ill_formed_scores_are_refused() {
        let key = attestor();

        let mut zero_subject = sample();
        zero_subject.subject = [0u8; 32];
        assert_eq!(zero_subject.validate(), Err(ReputationError::ZeroSubject));
        assert_eq!(
            zero_subject.sign(&key).unwrap_err(),
            ReputationError::ZeroSubject
        );

        let mut zero_deadline = sample();
        zero_deadline.valid_until = 0;
        assert_eq!(
            zero_deadline.validate(),
            Err(ReputationError::ZeroValidUntil)
        );

        let mut zero_anchor = sample();
        zero_anchor.solana_attestation = [0u8; 32];
        assert_eq!(
            zero_anchor.validate(),
            Err(ReputationError::ZeroSolanaAttestation)
        );

        let mut no_source = sample();
        no_source.source_chain = String::new();
        assert_eq!(no_source.validate(), Err(ReputationError::EmptySourceChain));
    }

    #[test]
    fn score_above_full_scale_is_refused() {
        let over = ReputationAttestation {
            score: 10_001,
            score_decimals: 4,
            ..sample()
        };
        assert_eq!(
            over.validate(),
            Err(ReputationError::ScoreOutOfRange {
                score: 10_001,
                max: 10_000,
                decimals: 4
            })
        );
        // Full marks — exactly 1.0 at scale — is the boundary and is allowed.
        let perfect = ReputationAttestation {
            score: 10_000,
            score_decimals: 4,
            ..sample()
        };
        assert!(perfect.validate().is_ok());
    }

    #[test]
    fn absurd_decimals_are_refused_before_overflow() {
        let att = ReputationAttestation {
            score_decimals: 19,
            ..sample()
        };
        assert_eq!(
            att.validate(),
            Err(ReputationError::ScoreDecimalsTooLarge {
                decimals: 19,
                max: 18
            })
        );
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
            Err(ReputationError::Signature(
                "non-canonical high-S signature".into()
            ))
        );
    }

    // Frozen cross-language vector. `CovenantReputationRegistry.t.sol::
    // test_Golden_ReputationDigest` pins the same typehash, domain separator,
    // and digest from these exact inputs; if either side drifts, one of these
    // literals stops matching. Regenerate with
    // `cargo run -p covenant-attestation --example reputation_vector`.
    #[test]
    fn eip712_encoding_is_pinned() {
        assert_eq!(
            hex_encode(&keccak256(EIP712_DOMAIN_TYPE)),
            "599a80fcaa47b95e2323ab4d34d34e0cc9feda4b843edafcc30c7bdf60ea15bf"
        );
        assert_eq!(
            hex_encode(&keccak256(REPUTATION_TYPE)),
            "583a7d61419e1a73ece092df468623437abc7458cc4580e398e81c70a6a691e8"
        );

        let golden = ReputationAttestation {
            subject: [0xAB; 32],
            score: 9_500,
            score_decimals: 4,
            valid_until: 1_700_003_600,
            source_chain: SOURCE_CHAIN_SOLANA.to_string(),
            solana_attestation: [0x22; 32],
        };
        // A constant, chain-agnostic domain: the digest transitively pins the
        // struct hash, so any field-layout drift also breaks this vector.
        assert_eq!(
            hex_encode(&ReputationAttestation::domain_separator()),
            "a1810486e59f4b39150c8c9cf9944cf3cf07150d1371650d7eb96d1b71e562fb"
        );
        assert_eq!(
            hex_encode(&golden.digest()),
            "e9a0fe2c860337d88659e7a68324eb92f8942f0ca36a0742aa3c45ee4dcccef5"
        );
    }
}
