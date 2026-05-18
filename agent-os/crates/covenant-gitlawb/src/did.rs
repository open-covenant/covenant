//! `did:key` derivation for ed25519 keys, matching gitlawb-core's encoding.
//!
//! gitlawb encodes an actor's ed25519 verifying key as
//! `did:key:z<base58btc(0xed 0x01 || pubkey)>`, the multibase prefix `z` for
//! base58btc, then the ed25519-pub multicodec varint (`0xed 0x01`), then the
//! 32-byte key. Any Covenant signer encoded this way produces the exact same
//! DID that gitlawb-core would produce from the same key, so an agent has one
//! identity across both systems.

use ed25519_dalek::VerifyingKey;

use crate::{GitlawbError, Result};

/// Multicodec varint for the `ed25519-pub` codec.
const ED25519_MULTICODEC: [u8; 2] = [0xed, 0x01];

/// Encode an ed25519 verifying key as a `did:key`.
pub fn did_key_from_verifying_key(key: &VerifyingKey) -> String {
    let mut buf = Vec::with_capacity(ED25519_MULTICODEC.len() + 32);
    buf.extend_from_slice(&ED25519_MULTICODEC);
    buf.extend_from_slice(&key.to_bytes());
    format!("did:key:z{}", bs58::encode(&buf).into_string())
}

/// Parse a `did:key:z...` back into an ed25519 verifying key.
///
/// Returns [`GitlawbError::Did`] if the DID is not `did:key`, the multibase
/// prefix is wrong, the multicodec prefix is wrong, or the key bytes do not
/// decode as a valid ed25519 verifying key.
pub fn verifying_key_from_did_key(did: &str) -> Result<VerifyingKey> {
    let method_id = did
        .strip_prefix("did:key:")
        .ok_or_else(|| GitlawbError::Did(format!("not a did:key: {did}")))?;

    let encoded = method_id
        .strip_prefix('z')
        .ok_or_else(|| GitlawbError::Did("did:key missing base58btc 'z' prefix".into()))?;

    let bytes = bs58::decode(encoded)
        .into_vec()
        .map_err(|e| GitlawbError::Did(format!("base58 decode: {e}")))?;

    if bytes.len() != ED25519_MULTICODEC.len() + 32 {
        return Err(GitlawbError::Did(format!(
            "expected {} bytes after decode, got {}",
            ED25519_MULTICODEC.len() + 32,
            bytes.len()
        )));
    }

    if bytes[..ED25519_MULTICODEC.len()] != ED25519_MULTICODEC {
        return Err(GitlawbError::Did("not an ed25519 multicodec key".into()));
    }

    let key_bytes: [u8; 32] = bytes[ED25519_MULTICODEC.len()..]
        .try_into()
        .expect("length checked above");

    VerifyingKey::from_bytes(&key_bytes)
        .map_err(|e| GitlawbError::Did(format!("invalid ed25519 key: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;
    use rand::TryRngCore;

    fn fresh_signing_key() -> SigningKey {
        let mut seed = [0u8; 32];
        OsRng.try_fill_bytes(&mut seed).unwrap();
        SigningKey::from_bytes(&seed)
    }

    #[test]
    fn did_key_starts_with_z6mk() {
        let sk = fresh_signing_key();
        let did = did_key_from_verifying_key(&sk.verifying_key());
        // ed25519 multicodec + 32-byte pubkey always base58btc-encodes to a
        // string starting with `z6Mk` (5-byte common prefix locked by the
        // 0xed 0x01 codec). Cross-checked with gitlawb-core's `did_key_round_trip` test.
        assert!(did.starts_with("did:key:z6Mk"), "{did}");
    }

    #[test]
    fn did_key_round_trip() {
        let sk = fresh_signing_key();
        let vk = sk.verifying_key();
        let did = did_key_from_verifying_key(&vk);
        let recovered = verifying_key_from_did_key(&did).unwrap();
        assert_eq!(vk.to_bytes(), recovered.to_bytes());
    }

    #[test]
    fn rejects_non_did_key() {
        let err = verifying_key_from_did_key("did:web:example.com").unwrap_err();
        assert!(matches!(err, GitlawbError::Did(_)));
    }

    #[test]
    fn rejects_wrong_multibase_prefix() {
        // 'm' is base64-multibase, not base58btc
        let err = verifying_key_from_did_key("did:key:mAAAA").unwrap_err();
        assert!(matches!(err, GitlawbError::Did(_)));
    }

    #[test]
    fn rejects_truncated_payload() {
        // 'z' prefix but base58 decodes to too few bytes
        let err = verifying_key_from_did_key("did:key:zAA").unwrap_err();
        assert!(matches!(err, GitlawbError::Did(_)));
    }
}
