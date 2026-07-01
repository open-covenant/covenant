//! The narrow slice of multibase/multicodec the multichain layer speaks:
//! `z…` base58-btc for proof values and `z6Mk…` `did:key` verification
//! methods. No multibase crate is pulled in — base58-btc is the only
//! alphabet we need and `bs58` already ships it. Also holds the plain
//! lowercase hex used to move the 32-byte audit root between its
//! on-chain hex form and the EIP-712 word.

use crate::AttestationError;

/// Multicodec header for an Ed25519 public key (`0xed`, varint `0xed 0x01`).
const ED25519_PUB_MULTICODEC: [u8; 2] = [0xed, 0x01];
/// Multicodec header for an Ed25519 secret key (`0x1300`, varint `0x80 0x26`).
/// Only the conformance vector replays a private key from multibase.
#[cfg(test)]
const ED25519_PRIV_MULTICODEC: [u8; 2] = [0x80, 0x26];

/// Multibase base58-btc: the `z` prefix over the Bitcoin-alphabet base58.
pub fn base58btc(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(1 + bytes.len() * 2);
    out.push('z');
    out.push_str(&bs58::encode(bytes).into_string());
    out
}

fn base58btc_decode(value: &str) -> Result<Vec<u8>, AttestationError> {
    let body = value
        .strip_prefix('z')
        .ok_or_else(|| AttestationError::Multibase("expected 'z' base58-btc prefix".into()))?;
    bs58::decode(body)
        .into_vec()
        .map_err(|e| AttestationError::Multibase(e.to_string()))
}

/// Decode a base58-btc multibase `proofValue` into the 64-byte Ed25519
/// signature it carries.
pub fn base58btc_signature(value: &str) -> Result<[u8; 64], AttestationError> {
    let raw = base58btc_decode(value)?;
    raw.as_slice()
        .try_into()
        .map_err(|_| AttestationError::Multibase(format!("expected 64 signature bytes, got {}", raw.len())))
}

/// The `publicKeyMultibase` for an Ed25519 key: `z` + base58-btc of
/// (`0xed01` multicodec ‖ 32-byte key).
pub fn ed25519_public_key_multibase(pubkey: &[u8; 32]) -> String {
    let mut buf = Vec::with_capacity(2 + 32);
    buf.extend_from_slice(&ED25519_PUB_MULTICODEC);
    buf.extend_from_slice(pubkey);
    base58btc(&buf)
}

/// The `did:key` DID for an Ed25519 key (no fragment), used as the
/// credential `issuer`.
pub fn ed25519_did(pubkey: &[u8; 32]) -> String {
    format!("did:key:{}", ed25519_public_key_multibase(pubkey))
}

/// The `did:key` verification method for an Ed25519 key,
/// `did:key:z6Mk…#z6Mk…`, self-describing so verification needs no
/// external DID resolution.
pub fn ed25519_did_key(pubkey: &[u8; 32]) -> String {
    let key = ed25519_public_key_multibase(pubkey);
    format!("did:key:{key}#{key}")
}

/// Recover the 32-byte Ed25519 public key from a verification method.
/// Accepts the `did:key:z…#z…` form and the bare `z6Mk…` multibase form
/// by taking the last `:`/`#`-delimited token.
pub fn ed25519_public_key_from_verification_method(
    vm: &str,
) -> Result<[u8; 32], AttestationError> {
    let token = vm
        .rsplit(|c| c == '#' || c == ':')
        .next()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| AttestationError::VerificationMethod(vm.to_string()))?;
    let raw = base58btc_decode(token)?;
    if raw.len() != 2 + 32 || raw[..2] != ED25519_PUB_MULTICODEC {
        return Err(AttestationError::Multicodec);
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&raw[2..]);
    Ok(key)
}

/// Decode the 32-byte Ed25519 seed from a `secretKeyMultibase` (`z…`,
/// multicodec `0x8026`). Used to replay the W3C conformance vector.
#[cfg(test)]
pub fn ed25519_seed_from_secret_multibase(value: &str) -> Result<[u8; 32], AttestationError> {
    let raw = base58btc_decode(value)?;
    if raw.len() != 2 + 32 || raw[..2] != ED25519_PRIV_MULTICODEC {
        return Err(AttestationError::Multicodec);
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&raw[2..]);
    Ok(seed)
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Lowercase hex, matching the audit root's on-chain string form.
pub fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn hex_nibble(c: u8) -> Result<u8, AttestationError> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        _ => Err(AttestationError::Hex(format!(
            "non-lowercase-hex byte {c:#04x}"
        ))),
    }
}

/// Decode lowercase hex of any even length. `0x` prefixes are the
/// caller's responsibility to strip.
pub fn hex_decode(value: &str) -> Result<Vec<u8>, AttestationError> {
    let bytes = value.as_bytes();
    if bytes.len() % 2 != 0 {
        return Err(AttestationError::Hex("odd hex length".into()));
    }
    bytes
        .chunks_exact(2)
        .map(|pair| Ok((hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?))
        .collect()
}

/// Decode exactly 32 bytes from a 64-character lowercase hex string. The
/// audit root is anchored as lowercase hex, so uppercase is rejected
/// rather than silently accepted — a differing case would still decode
/// to the same bytes but is not the anchored form.
pub fn hex_decode_32(value: &str) -> Result<[u8; 32], AttestationError> {
    let bytes = value.as_bytes();
    if bytes.len() != 64 {
        return Err(AttestationError::Hex(format!(
            "expected 64 hex characters, got {}",
            bytes.len()
        )));
    }
    let mut out = [0u8; 32];
    for (i, pair) in bytes.chunks_exact(2).enumerate() {
        out[i] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Ok(out)
}
