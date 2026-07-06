//! The narrow slice of EVM encoding the EAS off-chain layer needs:
//! Keccak-256, the two ABI head-words (`uint256`, left-padded `address`),
//! the pubkey→address derivation `ecrecover` implies, and lowercase hex.
//! No `hex`/`ethabi` crate is pulled in — EAS off-chain attestations only
//! touch fixed-width words plus one `keccak256(bytes)`, so hand-rolling it
//! keeps the sidecar's dependency surface as small as its blast radius.

use k256::ecdsa::VerifyingKey;
use sha3::{Digest, Keccak256};

/// The all-zero 20-byte address. EAS off-chain attestations to no
/// particular EVM recipient use it, and it is the resolver slot for a
/// schema registered without a resolver.
pub const ZERO_ADDRESS: [u8; 20] = [0u8; 20];

/// The EAS contract, an OP-Stack predeploy at the same address across the
/// Superchain — Base and Base Sepolia both host EAS here, differing only by
/// `chainId`. Both the off-chain domain and the on-chain relayer point at it.
pub const EAS_PREDEPLOY: [u8; 20] = [
    0x42, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x21,
];

pub fn keccak256(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Keccak256::digest(bytes));
    out
}

/// A `uint256` ABI word: 32-byte big-endian, left-padded with zeros.
pub fn word_u256(value: u64) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[24..].copy_from_slice(&value.to_be_bytes());
    word
}

/// An `address` ABI word: the 20 address bytes right-aligned in 32.
pub fn word_address(address: &[u8; 20]) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[12..].copy_from_slice(address);
    word
}

/// The Ethereum address of a secp256k1 public key: the low 20 bytes of
/// `keccak256` over the 64-byte uncompressed key (dropping the `0x04`
/// SEC1 tag). The same derivation `ecrecover` performs.
pub fn address_from_verifying_key(vk: &VerifyingKey) -> [u8; 20] {
    let encoded = vk.to_encoded_point(false);
    let hash = keccak256(&encoded.as_bytes()[1..]);
    let mut address = [0u8; 20];
    address.copy_from_slice(&hash[12..]);
    address
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Lowercase hex, no `0x` prefix.
pub fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Lowercase hex with a `0x` prefix — the on-wire form for a `bytes32`
/// UID or a `bytes` blob in an EAS attestation.
pub fn hex_0x(bytes: &[u8]) -> String {
    format!("0x{}", hex_encode(bytes))
}

fn nibble(c: u8) -> Result<u8, String> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(format!("non-hex byte {c:#04x}")),
    }
}

/// Decode hex to bytes, tolerating an optional `0x` prefix and mixed case
/// — the input side accepts whatever an EVM tool emits.
pub fn hex_decode(value: &str) -> Result<Vec<u8>, String> {
    let body = value.strip_prefix("0x").unwrap_or(value);
    if !body.len().is_multiple_of(2) {
        return Err("odd hex length".into());
    }
    body.as_bytes()
        .chunks_exact(2)
        .map(|pair| Ok((nibble(pair[0])? << 4) | nibble(pair[1])?))
        .collect()
}

/// Decode exactly 32 bytes from hex (optional `0x`), for a `bytes32` word.
pub fn hex_decode_32(value: &str) -> Result<[u8; 32], String> {
    let bytes = hex_decode(value)?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| format!("expected 32 bytes, got {}", bytes.len()))
}

/// Decode exactly 20 bytes from hex (optional `0x`), for an `address`.
/// Only the test vectors parse an address back from hex; production reads
/// addresses as bytes straight off credential verification.
#[cfg(test)]
pub fn hex_decode_20(value: &str) -> Result<[u8; 20], String> {
    let bytes = hex_decode(value)?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| format!("expected 20 bytes, got {}", bytes.len()))
}
