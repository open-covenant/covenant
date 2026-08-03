//! The bond-receipt attestation (multichain-30).
//!
//! A default agent bond is the chain-local settlement stablecoin — USDC on
//! Base, USDG (Global Dollar) on Robinhood Chain — never `$CVNT` (you do
//! not secure a token with itself; UMA and Polymarket post USDC). Covenant
//! confirms a posted bond with a secp256k1 signature over an EIP-712
//! [`BondReceipt`] digest, so the settlement chain's verifier authenticates
//! it with one `ecrecover` (~3k gas): no bridge, light client, or Solana
//! read on the verification path.
//!
//! Unlike the credential attestation ([`crate::eip712`]), whose domain is
//! chain-agnostic (`salt`), a bond exists on exactly one chain with one
//! token contract, so this domain binds `chainId`: a Base-Sepolia receipt
//! must not verify against a Base-mainnet verifier, nor a Base receipt
//! against the Robinhood-Chain one. `verifyingContract` is deliberately
//! omitted so a receipt stays valid across verifier redeploys on the same
//! chain (verifiers are live on Base and Robinhood Chain mainnet, see
//! `agent-os/evm/deployments.json`); an operator may bind it at deploy time.
//!
//! The security hinge is the ed25519 ↔ secp256k1 binding (bidirectionally
//! anchored on Solana by multichain-00): [`BondReceipt::subject`] carries
//! the agent's Solana identity as `bytes32`, committed inside the signed
//! struct hash. A receipt captured for one agent cannot be re-presented
//! as another — swapping `subject` changes the digest, so `ecrecover` no
//! longer returns Covenant's attestor. That same `subject`=Solana-identity
//! commitment is what lets a Robinhood-Chain bond carry the canonical
//! Solana identity onto the settlement chain without a second registry.

use covenant_identity::Secp256k1IssuerKey;
use k256::ecdsa::{RecoveryId, Signature as EcdsaSignature, VerifyingKey};
use sha3::{Digest, Keccak256};

/// Basis-points denominator. A slash split of exactly 10 000 bps routes
/// 100% to one party; the invariant is `0 < bps < BPS_DENOMINATOR`, so a
/// remainder always leaves the beneficiary — the party that can trigger a
/// slash never collects the whole bond.
pub const BPS_DENOMINATOR: u16 = 10_000;

const DOMAIN_NAME: &str = "Covenant Bond Receipt";
const DOMAIN_VERSION: &str = "1";
const EIP712_DOMAIN_TYPE: &[u8] = b"EIP712Domain(string name,string version,uint256 chainId)";
const BOND_RECEIPT_TYPE: &[u8] = b"BondReceipt(bytes32 subject,address bondToken,uint256 bondAmount,address agentReturn,address slashBeneficiary,uint256 slashBeneficiaryBps,bytes32 nonce,uint256 issuedAt,uint256 expiry)";

/// A chain a bond can settle on, each pinned to the stablecoin it is
/// denominated in — Circle USDC on Base, Global Dollar (USDG) on Robinhood
/// Chain. Both are 6-decimal EIP-3009 stablecoins; never `$CVNT` (you do
/// not secure a token with itself). Construction refuses any other bond
/// token, closing the wrong-token-per-network failure mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BondNetwork {
    BaseMainnet,
    BaseSepolia,
    RobinhoodMainnet,
}

impl BondNetwork {
    pub fn chain_id(self) -> u64 {
        match self {
            BondNetwork::BaseMainnet => 8453,
            BondNetwork::BaseSepolia => 84_532,
            BondNetwork::RobinhoodMainnet => 4663,
        }
    }

    /// The canonical settlement stablecoin on this chain, pinned to its hex
    /// form by `bond_tokens_match_pinned_hex`. USDC on Base, USDG on
    /// Robinhood Chain — both 6 decimals.
    pub fn bond_token(self) -> [u8; 20] {
        match self {
            // 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913
            BondNetwork::BaseMainnet => [
                0x83, 0x35, 0x89, 0xfc, 0xd6, 0xed, 0xb6, 0xe0, 0x8f, 0x4c, 0x7c, 0x32, 0xd4, 0xf7,
                0x1b, 0x54, 0xbd, 0xa0, 0x29, 0x13,
            ],
            // 0x036CbD53842c5426634e7929541eC2318f3dCF7e
            BondNetwork::BaseSepolia => [
                0x03, 0x6c, 0xbd, 0x53, 0x84, 0x2c, 0x54, 0x26, 0x63, 0x4e, 0x79, 0x29, 0x54, 0x1e,
                0xc2, 0x31, 0x8f, 0x3d, 0xcf, 0x7e,
            ],
            // 0x5fc5360D0400a0Fd4f2af552ADD042D716F1d168 — USDG (Global Dollar, Paxos)
            BondNetwork::RobinhoodMainnet => [
                0x5f, 0xc5, 0x36, 0x0d, 0x04, 0x00, 0xa0, 0xfd, 0x4f, 0x2a, 0xf5, 0x52, 0xad, 0xd0,
                0x42, 0xd7, 0x16, 0xf1, 0xd1, 0x68,
            ],
        }
    }
}

/// Every way a bond receipt fails to construct, sign, or verify. Each
/// variant names the specific check so a reviewer can tell a forged
/// receipt from a malformed one.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BondError {
    #[error("bond token is not the USDC contract for this chain")]
    TokenMismatch,
    #[error("bond amount must be greater than zero")]
    ZeroBondAmount,
    #[error("subject identity is all-zero")]
    ZeroSubject,
    #[error("{0} address is all-zero")]
    ZeroAddress(&'static str),
    #[error("slash beneficiary must not be the bonded agent's return address")]
    SelfSlash,
    #[error("slash split must be 1..=9999 bps: never 0% or 100% to one party")]
    SlashSplit,
    #[error("validity window is inverted: expiry {expiry} <= issuedAt {issued_at}")]
    WindowInverted { issued_at: u64, expiry: u64 },
    #[error("bond receipt expired: now {now} > expiry {expiry}")]
    Expired { now: u64, expiry: u64 },
    #[error("recovered signer is not the trusted attestor")]
    UntrustedSigner,
    #[error("malformed signature: {0}")]
    Signature(String),
}

/// A Covenant-signed statement that an agent posted a chain-local
/// stablecoin bond — USDC on Base, USDG on Robinhood Chain. Every field is
/// committed into the EIP-712 digest, so the on-chain verifier
/// authenticates all of them with one `ecrecover`.
///
/// Constructed as a literal (like [`crate::AttestationInput`]); the fields
/// are re-checked by [`BondReceipt::validate`], which [`BondReceipt::sign`]
/// runs before signing so an invalid literal is never signed.
#[derive(Debug, Clone)]
pub struct BondReceipt {
    pub network: BondNetwork,
    /// The bonded agent's Solana identity (ed25519 key / PDA) as bytes32 —
    /// the ed25519 ↔ secp256k1 binding leg.
    pub subject: [u8; 32],
    /// The bond token; must equal `network.bond_token()`.
    pub bond_token: [u8; 20],
    /// Bond size in the settlement token's smallest unit (USDC and USDG are
    /// both 6 decimals).
    pub bond_amount: u128,
    /// Where the bond returns on honest exit.
    pub agent_return: [u8; 20],
    /// Where the beneficiary share of a slash routes.
    pub slash_beneficiary: [u8; 20],
    /// Beneficiary share of a slash, in bps; `0 < bps < 10000`.
    pub slash_beneficiary_bps: u16,
    /// Bond id / replay nonce.
    pub nonce: [u8; 32],
    pub issued_at: u64,
    pub expiry: u64,
}

impl BondReceipt {
    /// Fail-closed field checks, independent of any signature. Run at sign
    /// time and again inside [`SignedBondReceipt::verify`], so a receipt
    /// deserialized from the wire is re-checked before it is trusted.
    pub fn validate(&self) -> Result<(), BondError> {
        if self.bond_token != self.network.bond_token() {
            return Err(BondError::TokenMismatch);
        }
        if self.bond_amount == 0 {
            return Err(BondError::ZeroBondAmount);
        }
        if self.subject == [0u8; 32] {
            return Err(BondError::ZeroSubject);
        }
        if self.agent_return == [0u8; 20] {
            return Err(BondError::ZeroAddress("agentReturn"));
        }
        if self.slash_beneficiary == [0u8; 20] {
            return Err(BondError::ZeroAddress("slashBeneficiary"));
        }
        if self.slash_beneficiary == self.agent_return {
            return Err(BondError::SelfSlash);
        }
        if self.slash_beneficiary_bps == 0 || self.slash_beneficiary_bps >= BPS_DENOMINATOR {
            return Err(BondError::SlashSplit);
        }
        if self.expiry <= self.issued_at {
            return Err(BondError::WindowInverted {
                issued_at: self.issued_at,
                expiry: self.expiry,
            });
        }
        Ok(())
    }

    /// `keccak256(abi.encode(typeHash, keccak(name), keccak(version), chainId))`.
    pub fn domain_separator(&self) -> [u8; 32] {
        let mut buf = Vec::with_capacity(32 * 4);
        buf.extend_from_slice(&keccak256(EIP712_DOMAIN_TYPE));
        buf.extend_from_slice(&keccak256(DOMAIN_NAME.as_bytes()));
        buf.extend_from_slice(&keccak256(DOMAIN_VERSION.as_bytes()));
        buf.extend_from_slice(&uint256(u128::from(self.network.chain_id())));
        keccak256(&buf)
    }

    fn struct_hash(&self) -> [u8; 32] {
        let mut buf = Vec::with_capacity(32 * 10);
        buf.extend_from_slice(&keccak256(BOND_RECEIPT_TYPE));
        buf.extend_from_slice(&self.subject);
        buf.extend_from_slice(&address_word(&self.bond_token));
        buf.extend_from_slice(&uint256(self.bond_amount));
        buf.extend_from_slice(&address_word(&self.agent_return));
        buf.extend_from_slice(&address_word(&self.slash_beneficiary));
        buf.extend_from_slice(&uint256(u128::from(self.slash_beneficiary_bps)));
        buf.extend_from_slice(&self.nonce);
        buf.extend_from_slice(&uint256(u128::from(self.issued_at)));
        buf.extend_from_slice(&uint256(u128::from(self.expiry)));
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

    /// Validate, then sign with Covenant's secp256k1 issuer key. Returns
    /// an error rather than signing an invalid receipt.
    pub fn sign(&self, issuer: &Secp256k1IssuerKey) -> Result<SignedBondReceipt, BondError> {
        self.validate()?;
        let signature = issuer.sign_eip712_digest(&self.digest());
        Ok(SignedBondReceipt {
            receipt: self.clone(),
            signature,
        })
    }
}

/// A [`BondReceipt`] plus the attestor's 65-byte `r ‖ s ‖ v` signature —
/// the artifact an agent presents to a Base verifier.
#[derive(Debug, Clone)]
pub struct SignedBondReceipt {
    receipt: BondReceipt,
    signature: [u8; 65],
}

impl SignedBondReceipt {
    pub fn receipt(&self) -> &BondReceipt {
        &self.receipt
    }

    /// The 65-byte `r ‖ s ‖ v` signature, low-S normalized, `v ∈ {27, 28}`.
    pub fn signature(&self) -> &[u8; 65] {
        &self.signature
    }

    pub fn digest(&self) -> [u8; 32] {
        self.receipt.digest()
    }

    /// Recover the signer address from the signature over the digest.
    pub fn recover_signer(&self) -> Result<[u8; 20], BondError> {
        recover_address(&self.receipt.digest(), &self.signature)
    }

    /// The full fail-closed check, exactly as the reference verifier
    /// performs it: re-validate the fields, reject an expired receipt
    /// against `now` (inclusive of `expiry`), and require the recovered
    /// signer to be the trusted attestor.
    pub fn verify(&self, trusted_attestor: &[u8; 20], now: u64) -> Result<(), BondError> {
        self.receipt.validate()?;
        if now > self.receipt.expiry {
            return Err(BondError::Expired {
                now,
                expiry: self.receipt.expiry,
            });
        }
        if &self.recover_signer()? != trusted_attestor {
            return Err(BondError::UntrustedSigner);
        }
        Ok(())
    }

    /// The 128-byte input to the `ecrecover` precompile (address `0x01`):
    /// `digest ‖ v ‖ r ‖ s`, each a 32-byte word, `v` right-aligned.
    /// Calling the precompile with this input returns the same address as
    /// [`recover_signer`]; the live dry-run confirms it on Base Sepolia.
    pub fn ecrecover_precompile_calldata(&self) -> [u8; 128] {
        let mut calldata = [0u8; 128];
        calldata[..32].copy_from_slice(&self.receipt.digest());
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

fn recover_address(digest: &[u8; 32], signature: &[u8; 65]) -> Result<[u8; 20], BondError> {
    // EVM `ecrecover` accepts only v ∈ {27, 28} (recid 0/1); reject recid 2/3
    // so this path agrees with the Base verifier's precompile.
    let recovery = signature[64]
        .checked_sub(27)
        .filter(|&b| b <= 1)
        .and_then(RecoveryId::from_byte)
        .ok_or_else(|| BondError::Signature(format!("bad recovery byte {}", signature[64])))?;
    let sig = EcdsaSignature::from_slice(&signature[..64])
        .map_err(|e| BondError::Signature(e.to_string()))?;
    // Reject the malleable high-S twin: Covenant only ever emits low-S
    // (see `Secp256k1IssuerKey::sign_eip712_digest`), so a non-canonical
    // signature is never a legitimate receipt. `ecrecover` itself cannot
    // reject it, so the off-chain issuer path canonicalizes here.
    if sig.normalize_s().is_some() {
        return Err(BondError::Signature(
            "non-canonical high-S signature".into(),
        ));
    }
    let key = VerifyingKey::recover_from_prehash(digest, &sig, recovery)
        .map_err(|e| BondError::Signature(e.to_string()))?;
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

    // The universally published keccak256 test vectors: the empty input,
    // and the standard full EIP712Domain typehash every EIP-712 signer
    // computes. Reproducing both proves this module's keccak256 is the
    // Ethereum keccak256 (not NIST SHA3-256) — a cross-implementation
    // anchor, not a self-consistency check.
    const KECCAK_EMPTY: &str = "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470";
    const PUBLISHED_FULL_DOMAIN_TYPEHASH: &str =
        "8b73c3c69bb8fe3d512ecc4cf759cc79239f7b179b0ffacaa9a75d522b39400f";

    fn attestor() -> Secp256k1IssuerKey {
        Secp256k1IssuerKey::from_secret_bytes(&[9u8; 32]).unwrap()
    }

    fn sample(network: BondNetwork) -> BondReceipt {
        BondReceipt {
            network,
            subject: [0xAB; 32],
            bond_token: network.bond_token(),
            bond_amount: 1_000_000,
            agent_return: [0x11; 20],
            slash_beneficiary: [0x22; 20],
            slash_beneficiary_bps: 8_000,
            nonce: [0xCD; 32],
            issued_at: 1_700_000_000,
            expiry: 1_800_000_000,
        }
    }

    #[test]
    fn keccak_reproduces_published_vectors() {
        assert_eq!(hex_encode(&keccak256(b"")), KECCAK_EMPTY);
        assert_eq!(
            hex_encode(&keccak256(
                b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"
            )),
            PUBLISHED_FULL_DOMAIN_TYPEHASH
        );
    }

    #[test]
    fn bond_tokens_match_pinned_hex() {
        assert_eq!(
            hex_encode(&BondNetwork::BaseMainnet.bond_token()),
            "833589fcd6edb6e08f4c7c32d4f71b54bda02913"
        );
        assert_eq!(
            hex_encode(&BondNetwork::BaseSepolia.bond_token()),
            "036cbd53842c5426634e7929541ec2318f3dcf7e"
        );
        // USDG (Global Dollar) on Robinhood Chain mainnet.
        assert_eq!(
            hex_encode(&BondNetwork::RobinhoodMainnet.bond_token()),
            "5fc5360d0400a0fd4f2af552add042d716f1d168"
        );
        assert_eq!(BondNetwork::BaseMainnet.chain_id(), 8453);
        assert_eq!(BondNetwork::BaseSepolia.chain_id(), 84_532);
        assert_eq!(BondNetwork::RobinhoodMainnet.chain_id(), 4663);
    }

    #[test]
    fn sign_then_verify_round_trip() {
        let key = attestor();
        let signed = sample(BondNetwork::BaseSepolia).sign(&key).unwrap();
        assert!(signed.verify(&key.address(), 1_750_000_000).is_ok());
        assert_eq!(signed.recover_signer().unwrap(), key.address());
        assert!(matches!(signed.signature()[64], 27 | 28)); // ecrecover v
    }

    #[test]
    fn expiry_is_enforced_inclusive() {
        let key = attestor();
        let signed = sample(BondNetwork::BaseSepolia).sign(&key).unwrap();
        let addr = key.address();
        // Accepted up to and including expiry; rejected one second past it.
        assert!(signed.verify(&addr, 1_800_000_000).is_ok());
        assert_eq!(
            signed.verify(&addr, 1_800_000_001),
            Err(BondError::Expired {
                now: 1_800_000_001,
                expiry: 1_800_000_000
            })
        );
    }

    #[test]
    fn binding_is_bound_into_the_signature() {
        let key = attestor();
        let signed = sample(BondNetwork::BaseSepolia).sign(&key).unwrap();
        // Re-present a valid receipt as a *different* agent by swapping the
        // subject, keeping the original signature. The digest changes, so
        // ecrecover no longer returns the attestor.
        let mut forged = signed.clone();
        forged.receipt.subject = [0xEE; 32];
        assert_eq!(
            forged.verify(&key.address(), 1_750_000_000),
            Err(BondError::UntrustedSigner)
        );
    }

    #[test]
    fn wrong_attestor_is_rejected() {
        let signed = sample(BondNetwork::BaseSepolia).sign(&attestor()).unwrap();
        let other = Secp256k1IssuerKey::from_secret_bytes(&[5u8; 32]).unwrap();
        assert_eq!(
            signed.verify(&other.address(), 1_750_000_000),
            Err(BondError::UntrustedSigner)
        );
    }

    #[test]
    fn corrupt_signature_is_rejected() {
        let key = attestor();
        let mut signed = sample(BondNetwork::BaseSepolia).sign(&key).unwrap();
        signed.signature[10] ^= 0xff;
        assert!(signed.verify(&key.address(), 1_750_000_000).is_err());
    }

    #[test]
    fn wrong_token_per_network_is_refused() {
        let key = attestor();
        // Base-mainnet USDC on a Base-Sepolia receipt: right token, wrong chain.
        let mut receipt = sample(BondNetwork::BaseSepolia);
        receipt.bond_token = BondNetwork::BaseMainnet.bond_token();
        assert_eq!(receipt.validate(), Err(BondError::TokenMismatch));
        assert_eq!(receipt.sign(&key).unwrap_err(), BondError::TokenMismatch);
    }

    #[test]
    fn self_slash_routing_is_refused() {
        // Slash proceeds may not route to the bonded agent's own address.
        let mut receipt = sample(BondNetwork::BaseSepolia);
        receipt.slash_beneficiary = receipt.agent_return;
        assert_eq!(receipt.validate(), Err(BondError::SelfSlash));

        // Nor 100% to one party, nor 0%.
        let mut full = sample(BondNetwork::BaseSepolia);
        full.slash_beneficiary_bps = BPS_DENOMINATOR;
        assert_eq!(full.validate(), Err(BondError::SlashSplit));
        let mut none = sample(BondNetwork::BaseSepolia);
        none.slash_beneficiary_bps = 0;
        assert_eq!(none.validate(), Err(BondError::SlashSplit));
    }

    #[test]
    fn zero_fields_and_inverted_window_are_refused() {
        let mut zero_subject = sample(BondNetwork::BaseSepolia);
        zero_subject.subject = [0u8; 32];
        assert_eq!(zero_subject.validate(), Err(BondError::ZeroSubject));

        let mut zero_return = sample(BondNetwork::BaseSepolia);
        zero_return.agent_return = [0u8; 20];
        // agent_return == slash_beneficiary check comes after the zero check
        // for agent_return, so this is ZeroAddress, not SelfSlash.
        assert_eq!(
            zero_return.validate(),
            Err(BondError::ZeroAddress("agentReturn"))
        );

        let mut zero_amount = sample(BondNetwork::BaseSepolia);
        zero_amount.bond_amount = 0;
        assert_eq!(zero_amount.validate(), Err(BondError::ZeroBondAmount));

        let mut inverted = sample(BondNetwork::BaseSepolia);
        inverted.expiry = inverted.issued_at;
        assert_eq!(
            inverted.validate(),
            Err(BondError::WindowInverted {
                issued_at: 1_700_000_000,
                expiry: 1_700_000_000
            })
        );
    }

    #[test]
    fn domain_binds_chain_id() {
        // The same logical bond on different chains produces different domain
        // separators and digests, so a receipt for one chain cannot verify
        // under another chain's verifier.
        let sepolia = sample(BondNetwork::BaseSepolia);
        let mut mainnet = sample(BondNetwork::BaseMainnet);
        mainnet.bond_token = BondNetwork::BaseMainnet.bond_token();
        let mut robinhood = sample(BondNetwork::RobinhoodMainnet);
        robinhood.bond_token = BondNetwork::RobinhoodMainnet.bond_token();
        assert_ne!(sepolia.domain_separator(), mainnet.domain_separator());
        assert_ne!(sepolia.domain_separator(), robinhood.domain_separator());
        assert_ne!(mainnet.domain_separator(), robinhood.domain_separator());
        assert_ne!(sepolia.digest(), mainnet.digest());
        assert_ne!(sepolia.digest(), robinhood.digest());
        assert_ne!(mainnet.digest(), robinhood.digest());
    }

    #[test]
    fn ecrecover_precompile_calldata_matches_local_recovery() {
        let key = attestor();
        let signed = sample(BondNetwork::BaseSepolia).sign(&key).unwrap();
        let calldata = signed.ecrecover_precompile_calldata();

        // Layout: digest ‖ v ‖ r ‖ s.
        assert_eq!(&calldata[..32], &signed.digest());
        assert_eq!(calldata[63], signed.signature()[64]);
        assert_eq!(&calldata[64..96], &signed.signature()[..32]);
        assert_eq!(&calldata[96..128], &signed.signature()[32..64]);

        // The precompile recovers (digest, v, r, s) the same way this crate
        // does; the live dry-run proves the on-chain precompile agrees.
        let mut sig = [0u8; 65];
        sig[..32].copy_from_slice(&calldata[64..96]);
        sig[32..64].copy_from_slice(&calldata[96..128]);
        sig[64] = calldata[63];
        let digest: [u8; 32] = calldata[..32].try_into().unwrap();
        assert_eq!(recover_address(&digest, &sig).unwrap(), key.address());
    }

    #[test]
    fn signing_is_deterministic() {
        let key = attestor();
        let a = sample(BondNetwork::BaseSepolia).sign(&key).unwrap();
        let b = sample(BondNetwork::BaseSepolia).sign(&key).unwrap();
        assert_eq!(a.signature(), b.signature());
        assert_eq!(a.digest(), b.digest());
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
        // The malleable twin (r, N-s, flipped v) recovers the same address on
        // a raw ecrecover, but we only ever emit low-S, so the canonical
        // verifier must refuse the non-canonical form.
        let key = attestor();
        let mut signed = sample(BondNetwork::BaseSepolia).sign(&key).unwrap();
        let mut s = [0u8; 32];
        s.copy_from_slice(&signed.signature[32..64]);
        signed.signature[32..64].copy_from_slice(&be_sub(&SECP256K1_N, &s));
        signed.signature[64] = if signed.signature[64] == 27 { 28 } else { 27 };
        assert_eq!(
            signed.verify(&key.address(), 1_750_000_000),
            Err(BondError::Signature(
                "non-canonical high-S signature".into()
            ))
        );
    }

    // Regression anchors: lock the EIP-712 encoding so an accidental change
    // to a type string, field order, or word packing is caught. The
    // reference verifier (agent-os/evm/contracts/BondReceiptVerifier.sol)
    // pins the same two typehashes.
    #[test]
    fn eip712_encoding_is_pinned() {
        // The two typehashes are cross-referenced by BondReceiptVerifier.sol.
        assert_eq!(
            hex_encode(&keccak256(EIP712_DOMAIN_TYPE)),
            "c2f8787176b8ac6bf7215b4adcc1e069bf4ab82d9ab1df05a57a91d425935b6e"
        );
        assert_eq!(
            hex_encode(&keccak256(BOND_RECEIPT_TYPE)),
            "2a1ad6a6ecf1ab273f9cd50e3f3a22f0eb1d9eb1dd75063a6a489aaa63415135"
        );

        // A fixed receipt's digest on each chain: distinct (chain-bound) and
        // locked against an accidental change to field order or word packing.
        let mut mainnet = sample(BondNetwork::BaseMainnet);
        mainnet.bond_token = BondNetwork::BaseMainnet.bond_token();
        assert_eq!(
            hex_encode(&sample(BondNetwork::BaseSepolia).digest()),
            "0a7473fbc69d50af1b19bc289d2b2d1f8b9c6fde83c3a39b5fc53fb87dc78abf"
        );
        assert_eq!(
            hex_encode(&mainnet.digest()),
            "170d48293404460b707729e14aedd9f59ac55737fd35a40007844cf93828e3b7"
        );
        let mut robinhood = sample(BondNetwork::RobinhoodMainnet);
        robinhood.bond_token = BondNetwork::RobinhoodMainnet.bond_token();
        assert_eq!(
            hex_encode(&robinhood.digest()),
            "399e55908e91ebece6739af5364ef571099e4859c11331e5795ceb1b1b67bcfa"
        );
    }
}
