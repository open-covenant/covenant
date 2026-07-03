//! The EAS off-chain attestation EIP-712 digest.
//!
//! An EAS off-chain attestation is an [EIP-712] typed-data signature over
//! an `Attest` struct, bound to a domain that names the chain (`chainId`)
//! and the EAS contract (`verifyingContract`). That binding is the whole
//! point: an EVM verifier — or EAS's own tooling — recovers the attester
//! with one `ecrecover`, and a signature made for one chain's EAS does not
//! replay against another's. This is deliberately *not* the salt-based
//! domain [`covenant_attestation`] uses for its portable VC proof; EAS
//! consumes the chain-scoped form, so we produce exactly that.
//!
//! This is [`OffchainAttestationVersion`][ver] 1: the `Attest` type leads
//! with a `uint16 version` and carries no `salt`. The field list, types,
//! and `0x1901` framing are pinned to two published EAS off-chain
//! attestations in the tests below — recomputing each digest and running
//! `ecrecover` must return the attester the EAS explorer records.
//!
//! [EIP-712]: https://eips.ethereum.org/EIPS/eip-712
//! [ver]: https://github.com/ethereum-attestation-service/eas-sdk

use k256::ecdsa::{RecoveryId, Signature as EcdsaSignature, VerifyingKey};

use crate::eth::{self, keccak256, word_address, word_u256};
use crate::EvmSignerError;

/// The EIP-712 domain `name` every EAS off-chain attestation shares.
/// (The on-chain delegated-attestation domain uses the bare string
/// `"EAS"`; off-chain uses this one.)
pub const DOMAIN_NAME: &str = "EAS Attestation";

/// The `OffchainAttestationVersion` this crate emits: `Version1`.
pub const OFFCHAIN_VERSION: u16 = 1;

const EIP712_DOMAIN_TYPE: &[u8] =
    b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)";
const ATTEST_TYPE: &[u8] = b"Attest(uint16 version,bytes32 schema,address recipient,uint64 time,uint64 expirationTime,bool revocable,bytes32 refUID,bytes data)";

/// The EAS off-chain EIP-712 domain: which chain, and which EAS contract,
/// the attestation is scoped to. `name` is fixed ([`DOMAIN_NAME`]); the
/// caller supplies the rest.
#[derive(Debug, Clone)]
pub struct EasDomain {
    /// The EAS deployment's `version()` string (e.g. `"1.2.0"` on Base).
    pub version: String,
    pub chain_id: u64,
    pub verifying_contract: [u8; 20],
}

impl EasDomain {
    fn separator(&self) -> [u8; 32] {
        let mut buf = Vec::with_capacity(32 * 5);
        buf.extend_from_slice(&keccak256(EIP712_DOMAIN_TYPE));
        buf.extend_from_slice(&keccak256(DOMAIN_NAME.as_bytes()));
        buf.extend_from_slice(&keccak256(self.version.as_bytes()));
        buf.extend_from_slice(&word_u256(self.chain_id));
        buf.extend_from_slice(&word_address(&self.verifying_contract));
        keccak256(&buf)
    }
}

/// The `Attest` message an off-chain attestation signs. `version` is fixed
/// at [`OFFCHAIN_VERSION`]; `schema` is the schema UID the payload
/// conforms to, and `data` is its ABI encoding.
#[derive(Debug, Clone)]
pub struct AttestMessage {
    pub schema: [u8; 32],
    pub recipient: [u8; 20],
    pub time: u64,
    pub expiration_time: u64,
    pub revocable: bool,
    pub ref_uid: [u8; 32],
    pub data: Vec<u8>,
}

impl AttestMessage {
    fn struct_hash(&self) -> [u8; 32] {
        let mut buf = Vec::with_capacity(32 * 9);
        buf.extend_from_slice(&keccak256(ATTEST_TYPE));
        buf.extend_from_slice(&word_u256(OFFCHAIN_VERSION as u64));
        buf.extend_from_slice(&self.schema);
        buf.extend_from_slice(&word_address(&self.recipient));
        buf.extend_from_slice(&word_u256(self.time));
        buf.extend_from_slice(&word_u256(self.expiration_time));
        buf.extend_from_slice(&word_u256(self.revocable as u64));
        buf.extend_from_slice(&self.ref_uid);
        // `bytes data` is a dynamic type: its struct-member encoding is
        // keccak256 over the raw bytes, not the bytes inline.
        buf.extend_from_slice(&keccak256(&self.data));
        keccak256(&buf)
    }
}

/// The EIP-712 signing digest: `keccak256(0x1901 ‖ domainSeparator ‖ structHash)`.
pub fn digest(domain: &EasDomain, message: &AttestMessage) -> [u8; 32] {
    let mut buf = Vec::with_capacity(2 + 32 + 32);
    buf.push(0x19);
    buf.push(0x01);
    buf.extend_from_slice(&domain.separator());
    buf.extend_from_slice(&message.struct_hash());
    keccak256(&buf)
}

/// Recover the 20-byte attester from a 65-byte `r ‖ s ‖ v` signature
/// (`v ∈ {27, 28}`) over `digest`. The inverse of what an EVM contract's
/// `ecrecover(digest, v, r, s)` computes.
pub fn recover_address(
    digest: &[u8; 32],
    signature: &[u8; 65],
) -> Result<[u8; 20], EvmSignerError> {
    let recovery = signature[64]
        .checked_sub(27)
        .and_then(RecoveryId::from_byte)
        .ok_or_else(|| EvmSignerError::Signature(format!("bad recovery byte {}", signature[64])))?;
    let ecdsa = EcdsaSignature::from_slice(&signature[..64])
        .map_err(|e| EvmSignerError::Signature(e.to_string()))?;
    let vk = VerifyingKey::recover_from_prehash(digest, &ecdsa, recovery)
        .map_err(|e| EvmSignerError::Signature(e.to_string()))?;
    Ok(eth::address_from_verifying_key(&vk))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eth::{hex_decode, hex_decode_20, hex_decode_32};

    /// A published EAS off-chain attestation: its message, domain, and
    /// signature, plus the attester the EAS explorer recovered. If our
    /// digest math is EAS-conformant, `ecrecover` over the recomputed
    /// digest returns exactly `signer`.
    struct Vector {
        version_str: &'static str,
        chain_id: u64,
        verifying_contract: &'static str,
        schema: &'static str,
        recipient: &'static str,
        time: u64,
        expiration_time: u64,
        revocable: bool,
        ref_uid: &'static str,
        data: &'static str,
        r: &'static str,
        s: &'static str,
        v: u8,
        signer: &'static str,
    }

    impl Vector {
        fn domain(&self) -> EasDomain {
            EasDomain {
                version: self.version_str.to_string(),
                chain_id: self.chain_id,
                verifying_contract: hex_decode_20(self.verifying_contract).unwrap(),
            }
        }

        fn message(&self) -> AttestMessage {
            AttestMessage {
                schema: hex_decode_32(self.schema).unwrap(),
                recipient: hex_decode_20(self.recipient).unwrap(),
                time: self.time,
                expiration_time: self.expiration_time,
                revocable: self.revocable,
                ref_uid: hex_decode_32(self.ref_uid).unwrap(),
                data: hex_decode(self.data).unwrap(),
            }
        }

        fn signature(&self) -> [u8; 65] {
            let mut sig = [0u8; 65];
            sig[..32].copy_from_slice(&hex_decode_32(self.r).unwrap());
            sig[32..64].copy_from_slice(&hex_decode_32(self.s).unwrap());
            sig[64] = self.v;
            sig
        }
    }

    // EAS docs "Storing Offchain Attestations" — mainnet "Make a
    // Statement" attestation, OffchainAttestationVersion 1.
    const MAINNET: Vector = Vector {
        version_str: "0.26",
        chain_id: 1,
        verifying_contract: "0xA1207F3BBa224E2c9c3c6D5aF63D0eb1582Ce587",
        schema: "0xf58b8b212ef75ee8cd7e8d803c37c03e0519890502d5e99ee2412aae1456cafe",
        recipient: "0x0000000000000000000000000000000000000000",
        time: 1_694_227_180,
        expiration_time: 0,
        revocable: true,
        ref_uid: "0x0000000000000000000000000000000000000000000000000000000000000000",
        data: "0x000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000495468697320697320616e206f6666636861696e206174746573746174696f6e2120436f6d706c6574656c7920707269766174652e2050617373656420706565722d746f2d706565722e0000000000000000000000000000000000000000000000",
        r: "0x948e36add4753524c1ff0224427bc964e1262c5239d73d749d44aaead6ae6320",
        s: "0x59f7d812eb10323204f115aa18315b238dac4da609b0b5aec4d5892a23d55029",
        v: 27,
        signer: "0xeee68aECeB4A9e9f328a46c39F50d83fA0239cDF",
    };

    // EAS docs "Verify Attestation" — Sepolia sample attestation,
    // OffchainAttestationVersion 1, same signer as the mainnet vector.
    const SEPOLIA: Vector = Vector {
        version_str: "0.26",
        chain_id: 11_155_111,
        verifying_contract: "0xC2679fBD37d54388Ce493F1DB75320D236e1815e",
        schema: "0x3969bb076acfb992af54d51274c5c868641ca5344e1aacd0b1f5e4f80ac0822f",
        recipient: "0x0000000000000000000000000000000000000000",
        time: 1_694_196_775,
        expiration_time: 0,
        revocable: true,
        ref_uid: "0x0000000000000000000000000000000000000000000000000000000000000000",
        data: "0x0000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000003c5468697320697320612073616d706c65206f6666636861696e206174746573746174696f6e20746861742063616e206265207665726966696564212000000000",
        r: "0x96148fae1b641d7c0750a46aa5775c1840433d69705ae6edfbd7740eff49cc55",
        s: "0x0e0283146242cdafb137b704b7ded3496b90241455b2ececc33c344515c364ce",
        v: 28,
        signer: "0xeee68aECeB4A9e9f328a46c39F50d83fA0239cDF",
    };

    fn assert_recovers(vector: &Vector) {
        let digest = digest(&vector.domain(), &vector.message());
        let recovered = recover_address(&digest, &vector.signature()).unwrap();
        assert_eq!(
            eth::hex_0x(&recovered).to_lowercase(),
            vector.signer.to_lowercase(),
            "recovered attester must match the published signer"
        );
    }

    #[test]
    fn mainnet_offchain_vector_recovers_to_published_signer() {
        assert_recovers(&MAINNET);
    }

    #[test]
    fn sepolia_offchain_vector_recovers_to_published_signer() {
        assert_recovers(&SEPOLIA);
    }

    #[test]
    fn a_wrong_domain_recovers_the_wrong_attester() {
        // The domain binding is load-bearing: change only the chainId and
        // the recovered address must no longer be the real signer. Guards
        // the "wrong EIP-712 domain -> wrong ecrecover" failure mode.
        let mut domain = MAINNET.domain();
        domain.chain_id = 8453; // Base mainnet, not the vector's chain 1
        let digest = digest(&domain, &MAINNET.message());
        let recovered = recover_address(&digest, &MAINNET.signature()).unwrap();
        assert_ne!(
            eth::hex_0x(&recovered).to_lowercase(),
            MAINNET.signer.to_lowercase()
        );
    }
}
