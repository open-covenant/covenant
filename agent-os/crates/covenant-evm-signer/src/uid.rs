//! EAS unique identifiers, as `keccak256` over an `abi.encodePacked`
//! preimage — the two UIDs an off-chain attestation carries.
//!
//! - [`schema_uid`] is what "the schema resolves" means for an off-chain
//!   attestation: the attestation references a schema by the UID EAS's
//!   `SchemaRegistry.getUID` derives from `(schema, resolver, revocable)`,
//!   so a resolver can look it up.
//! - [`offchain_uid`] is the attestation's own identifier, the value the
//!   EAS explorer indexes it under.
//!
//! Both mirror the EAS SDK's `solidityPackedKeccak256` calls exactly,
//! including the quirk that [`offchain_uid`] hashes the schema as the
//! UTF-8 bytes of its `0x…` string, not as the 32 raw bytes. The packed
//! encoder is proven by the golden-UID tests: two published attestations'
//! UIDs must reproduce byte-for-byte, and one published schema UID from
//! its `(string, resolver, bool)` triple.

use crate::eip712::{AttestMessage, OFFCHAIN_VERSION};
use crate::eth::{self, keccak256};

/// The EAS schema UID: `keccak256(abi.encodePacked(schema, resolver, revocable))`.
///
/// `schema` is the schema *definition* string (e.g.
/// `"bytes32 auditRoot,bytes32 credentialHash"`), packed as its raw UTF-8
/// bytes; `resolver` is the resolver contract (all-zero for none).
pub fn schema_uid(schema: &str, resolver: &[u8; 20], revocable: bool) -> [u8; 32] {
    let mut packed = Vec::with_capacity(schema.len() + 21);
    packed.extend_from_slice(schema.as_bytes());
    packed.extend_from_slice(resolver);
    packed.push(revocable as u8);
    keccak256(&packed)
}

/// The `OffchainAttestationVersion` 1 attestation UID, packing the message
/// exactly as the EAS SDK's `getOffchainUID`. The `bytes bytes` schema
/// member is the UTF-8 of the schema's `0x…` string; the second `address`
/// is the attester slot, which off-chain leaves zero; the trailing
/// `uint32(0)` is a fixed bump/nonce word.
pub fn offchain_uid(message: &AttestMessage) -> [u8; 32] {
    let schema_str = eth::hex_0x(&message.schema);
    let mut packed = Vec::with_capacity(2 + schema_str.len() + 20 + 20 + 8 + 8 + 1 + 32 + message.data.len() + 4);
    packed.extend_from_slice(&OFFCHAIN_VERSION.to_be_bytes());
    packed.extend_from_slice(schema_str.as_bytes());
    packed.extend_from_slice(&message.recipient);
    packed.extend_from_slice(&eth::ZERO_ADDRESS);
    packed.extend_from_slice(&message.time.to_be_bytes());
    packed.extend_from_slice(&message.expiration_time.to_be_bytes());
    packed.push(message.revocable as u8);
    packed.extend_from_slice(&message.ref_uid);
    packed.extend_from_slice(&message.data);
    packed.extend_from_slice(&0u32.to_be_bytes());
    keccak256(&packed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eth::{hex_decode, hex_decode_20, hex_decode_32};

    fn message(
        schema: &str,
        recipient: &str,
        time: u64,
        expiration_time: u64,
        revocable: bool,
        ref_uid: &str,
        data: &str,
    ) -> AttestMessage {
        AttestMessage {
            schema: hex_decode_32(schema).unwrap(),
            recipient: hex_decode_20(recipient).unwrap(),
            time,
            expiration_time,
            revocable,
            ref_uid: hex_decode_32(ref_uid).unwrap(),
            data: hex_decode(data).unwrap(),
        }
    }

    #[test]
    fn mainnet_offchain_uid_matches_published() {
        let msg = message(
            "0xf58b8b212ef75ee8cd7e8d803c37c03e0519890502d5e99ee2412aae1456cafe",
            "0x0000000000000000000000000000000000000000",
            1_694_227_180,
            0,
            true,
            "0x0000000000000000000000000000000000000000000000000000000000000000",
            "0x000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000495468697320697320616e206f6666636861696e206174746573746174696f6e2120436f6d706c6574656c7920707269766174652e2050617373656420706565722d746f2d706565722e0000000000000000000000000000000000000000000000",
        );
        assert_eq!(
            eth::hex_0x(&offchain_uid(&msg)),
            "0x9daf896fdad6ecb405f86d94395fecd4902461a2276180c6fa1d5c979bbf97d5"
        );
    }

    #[test]
    fn sepolia_offchain_uid_matches_published() {
        let msg = message(
            "0x3969bb076acfb992af54d51274c5c868641ca5344e1aacd0b1f5e4f80ac0822f",
            "0x0000000000000000000000000000000000000000",
            1_694_196_775,
            0,
            true,
            "0x0000000000000000000000000000000000000000000000000000000000000000",
            "0x0000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000003c5468697320697320612073616d706c65206f6666636861696e206174746573746174696f6e20746861742063616e206265207665726966696564212000000000",
        );
        assert_eq!(
            eth::hex_0x(&offchain_uid(&msg)),
            "0x268702f425d0704c4ad4677c6aded987827b0e74d33029162dbf245e0297c15f"
        );
    }

    #[test]
    fn make_a_statement_schema_uid_matches_published() {
        // EAS's canonical "Make a Statement" schema: `string statement`,
        // no resolver, revocable. Its published UID is the schema the
        // mainnet golden vector attests under.
        let uid = schema_uid("string statement", &eth::ZERO_ADDRESS, true);
        assert_eq!(
            eth::hex_0x(&uid),
            "0xf58b8b212ef75ee8cd7e8d803c37c03e0519890502d5e99ee2412aae1456cafe"
        );
    }

    #[test]
    fn schema_uid_binds_the_revocable_flag() {
        // Flipping revocable must change the UID — otherwise a schema
        // registered non-revocable would collide with a revocable one.
        let revocable = schema_uid("string statement", &eth::ZERO_ADDRESS, true);
        let non_revocable = schema_uid("string statement", &eth::ZERO_ADDRESS, false);
        assert_ne!(revocable, non_revocable);
    }
}
