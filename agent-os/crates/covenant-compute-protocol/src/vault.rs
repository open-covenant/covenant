//! Client-sealed secrets: a buyer encrypts a value under a key only they
//! hold, and the network stores the ciphertext. The store never receives
//! the key, so it can neither read a secret nor recover one from what it
//! keeps — a property that holds by construction, not by policy. This is
//! the delivery primitive the confidential tier is built on: a buyer can
//! stash an SSH key, a model token, or an env file where a coordinator
//! restart won't lose it, without trusting the coordinator with the
//! contents.
//!
//! The seal is XChaCha20-Poly1305 with a fresh random nonce per call.
//! The extended nonce is 192 bits, so drawing it at random needs no
//! per-key counter and no coordination — the collision probability over
//! any realistic number of secrets is negligible, which is exactly why
//! the extended-nonce variant exists. The key is the caller's to derive
//! and keep; this module deliberately offers only a random key and a
//! byte-exact round-trip, never a passphrase stretch, so no weak
//! key-derivation choice hides behind a convenient helper.
//!
//! The seal binds a caller-supplied context as associated data. The
//! buyer binds each secret to its `/vault/{owner}/secret/{label}` path,
//! so a store that swapped one owner's ciphertexts between labels is
//! caught at open — the tag alone would not catch it, since each
//! swapped blob is intact under its own nonce. The context is
//! authenticated, not stored: the sealed bytes on the wire are unchanged.
//!
//! Reads and writes to the store are authorized the same way the signed
//! buyer-history reads are (`read_auth`): the owner signs the request
//! path under a fixed domain and the store verifies possession of the
//! key the path names. A store additionally binds the ciphertext it is
//! writing, so a captured request cannot be replayed with a substituted
//! body to overwrite a secret in the signing window.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop};

use covenant_identity::LocalIdentity;

use crate::sign::{sign_domain, to_canonical_json, verify_domain, ProtocolError};

const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;

/// The largest plaintext a single secret may hold, before sealing. A
/// vault entry is meant for credentials and small config, not bulk data;
/// the store bounds what it will keep, and this bounds what a client will
/// seal so the two agree on the limit a caller hits.
pub const MAX_SECRET_BYTES: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("secret is {0} bytes, over the {MAX_SECRET_BYTES}-byte limit")]
    TooLarge(usize),
    #[error("sealed {field} is not valid base58")]
    Malformed { field: &'static str },
    #[error("sealed nonce is {0} bytes, expected {NONCE_LEN}")]
    NonceLength(usize),
    #[error("key is not a valid {KEY_LEN}-byte base58 value")]
    BadKey,
    /// Wrong key or tampered ciphertext — the two are indistinguishable
    /// to a verifier, which is the whole point of an authenticated seal.
    #[error("secret could not be opened with this key")]
    Decrypt,
}

/// A 32-byte symmetric key the caller holds and the store never sees.
/// Zeroized on drop so a key does not linger in freed memory; the
/// `Debug` impl redacts it so it cannot leak into a log line.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct VaultKey([u8; KEY_LEN]);

impl VaultKey {
    /// A fresh key from the OS CSPRNG. The caller keeps it: lose it and
    /// the ciphertext is unrecoverable, which is the guarantee, not a
    /// bug.
    pub fn random() -> Self {
        let mut key = [0u8; KEY_LEN];
        rand::rng().fill_bytes(&mut key);
        Self(key)
    }

    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    pub fn to_b58(&self) -> String {
        bs58::encode(self.0).into_string()
    }

    pub fn from_b58(value: &str) -> Result<Self, VaultError> {
        let bytes = bs58::decode(value)
            .into_vec()
            .map_err(|_| VaultError::BadKey)?;
        let key: [u8; KEY_LEN] = bytes.try_into().map_err(|_| VaultError::BadKey)?;
        Ok(Self(key))
    }
}

impl std::fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VaultKey(redacted)")
    }
}

/// A sealed secret as it crosses the wire and rests in the store: a
/// nonce and a ciphertext, both base58, and nothing else. There is no
/// field the store could use to open it — deliberately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedSecret {
    pub nonce: String,
    pub ciphertext: String,
}

/// One row of a secret listing: everything about a secret except the
/// secret. Both the store that produces it and the client that reads it
/// share this shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretMeta {
    pub label: String,
    /// Size of the stored base58 ciphertext, not the plaintext — the
    /// store never learns the plaintext length, and would not disclose it
    /// in a listing if it could.
    pub ciphertext_len: usize,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// Encrypts `plaintext` under `key` with a fresh nonce, binding `aad` as
/// associated data the matching [`open`] must supply byte for byte.
/// Refuses a plaintext over [`MAX_SECRET_BYTES`] so a caller learns the
/// limit here rather than at a store rejection.
pub fn seal(key: &VaultKey, plaintext: &[u8], aad: &[u8]) -> Result<SealedSecret, VaultError> {
    if plaintext.len() > MAX_SECRET_BYTES {
        return Err(VaultError::TooLarge(plaintext.len()));
    }
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_bytes()).expect("key is 32 bytes");
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = XNonce::from_slice(&nonce_bytes);
    // Poly1305 over an in-memory plaintext under the size cap above never
    // errors; treat a failure as the size guard the AEAD applies for us.
    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| VaultError::TooLarge(plaintext.len()))?;
    Ok(SealedSecret {
        nonce: bs58::encode(nonce_bytes).into_string(),
        ciphertext: bs58::encode(ciphertext).into_string(),
    })
}

/// Decrypts a sealed secret bound to `aad`. A wrong key, a tampered
/// nonce or ciphertext, or an `aad` that differs from the one the secret
/// was sealed under all fail the authentication tag and return
/// [`VaultError::Decrypt`] — none of them yield a plaintext.
pub fn open(key: &VaultKey, sealed: &SealedSecret, aad: &[u8]) -> Result<Vec<u8>, VaultError> {
    let nonce_bytes = bs58::decode(&sealed.nonce)
        .into_vec()
        .map_err(|_| VaultError::Malformed { field: "nonce" })?;
    if nonce_bytes.len() != NONCE_LEN {
        return Err(VaultError::NonceLength(nonce_bytes.len()));
    }
    let ciphertext =
        bs58::decode(&sealed.ciphertext)
            .into_vec()
            .map_err(|_| VaultError::Malformed {
                field: "ciphertext",
            })?;
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_bytes()).expect("key is 32 bytes");
    let nonce = XNonce::from_slice(&nonce_bytes);
    cipher
        .decrypt(
            nonce,
            Payload {
                msg: ciphertext.as_ref(),
                aad,
            },
        )
        .map_err(|_| VaultError::Decrypt)
}

/// The route path of a single secret, owner and label as the store keys
/// it. Both the client and the store format it here so a signature binds
/// the same string on each side.
pub fn vault_secret_path(owner_b58: &str, label: &str) -> String {
    format!("/vault/{owner_b58}/secret/{label}")
}

/// The route path of an owner's secret listing.
pub fn vault_list_path(owner_b58: &str) -> String {
    format!("/vault/{owner_b58}/secrets")
}

/// The exact string a vault request signs over: the HTTP method followed
/// by the route path. Binding the method is what stops a signature for
/// one operation from being replayed as another that shares a path — a
/// fetch reused as a delete — inside the signing window.
pub fn vault_signing_path(method: &str, route_path: &str) -> String {
    format!("{method} {route_path}")
}

pub const VAULT_DOMAIN: &[u8] = b"covenant.compute.vault.v1\n";
pub const VAULT_MAX_SKEW_MS: u64 = 120_000;
pub const VAULT_SIGNED_AT_HEADER: &str = "x-compute-vault-signed-at-ms";
pub const VAULT_SIGNATURE_HEADER: &str = "x-compute-vault-signature";

#[derive(Serialize)]
struct VaultClaim<'a> {
    path: &'a str,
    body_digest: String,
    signed_at_ms: u64,
}

/// A base58 SHA-256 of the request body, or of the empty string for a
/// request that carries none. Binding it into the signature is what
/// stops a captured store from being replayed with a different
/// ciphertext under the same path signature.
fn body_digest(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    bs58::encode(hasher.finalize()).into_string()
}

/// Signs a vault request over `path` and `body` at `signed_at_ms`. A
/// read (`body = &[]`) binds only the path; a store binds the exact
/// bytes it writes. Send the timestamp in [`VAULT_SIGNED_AT_HEADER`] and
/// the signature in [`VAULT_SIGNATURE_HEADER`].
pub fn sign_vault(
    identity: &LocalIdentity,
    path: &str,
    body: &[u8],
    signed_at_ms: u64,
) -> Result<String, ProtocolError> {
    let payload = to_canonical_json(&VaultClaim {
        path,
        body_digest: body_digest(body),
        signed_at_ms,
    })?;
    let (signature_b58, _) = sign_domain(identity, VAULT_DOMAIN, &payload);
    Ok(signature_b58)
}

/// Verifies that the holder of `expected_pubkey_b58` signed this exact
/// `path` and `body` within the skew window around `now_ms`.
pub fn verify_vault(
    expected_pubkey_b58: &str,
    path: &str,
    body: &[u8],
    signed_at_ms: u64,
    signature_b58: &str,
    now_ms: u64,
) -> Result<(), ProtocolError> {
    if now_ms.abs_diff(signed_at_ms) > VAULT_MAX_SKEW_MS {
        return Err(ProtocolError::Invalid(format!(
            "vault signature signed_at {signed_at_ms} is outside the \
             {VAULT_MAX_SKEW_MS}ms window around {now_ms}"
        )));
    }
    let payload = to_canonical_json(&VaultClaim {
        path,
        body_digest: body_digest(body),
        signed_at_ms,
    })?;
    verify_domain(VAULT_DOMAIN, &payload, signature_b58, expected_pubkey_b58)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AAD: &[u8] = b"/vault/owner/secret/deploy";

    #[test]
    fn a_sealed_secret_opens_back_to_its_plaintext() {
        let key = VaultKey::random();
        let plaintext = b"ssh-ed25519 AAAAC3Nz... buyer@laptop";
        let sealed = seal(&key, plaintext, AAD).unwrap();
        assert_eq!(open(&key, &sealed, AAD).unwrap(), plaintext);
    }

    #[test]
    fn an_empty_secret_round_trips() {
        let key = VaultKey::random();
        let sealed = seal(&key, b"", AAD).unwrap();
        assert!(open(&key, &sealed, AAD).unwrap().is_empty());
    }

    #[test]
    fn the_wrong_key_cannot_open_a_secret() {
        let key = VaultKey::random();
        let other = VaultKey::random();
        let sealed = seal(&key, b"api-token", AAD).unwrap();
        assert!(matches!(
            open(&other, &sealed, AAD),
            Err(VaultError::Decrypt)
        ));
    }

    #[test]
    fn a_secret_sealed_under_one_context_will_not_open_under_another() {
        let key = VaultKey::random();
        let sealed = seal(&key, b"deploy-token", b"/vault/o/secret/deploy").unwrap();
        // Same key, intact blob, only a different bound context: the swap
        // a store could attempt between an owner's own labels is refused,
        // not silently served back under the wrong label.
        assert!(matches!(
            open(&key, &sealed, b"/vault/o/secret/other"),
            Err(VaultError::Decrypt)
        ));
        assert_eq!(
            open(&key, &sealed, b"/vault/o/secret/deploy").unwrap(),
            b"deploy-token"
        );
    }

    #[test]
    fn a_tampered_ciphertext_or_nonce_is_rejected() {
        let key = VaultKey::random();
        let sealed = seal(&key, b"api-token", AAD).unwrap();

        let mut ct = bs58::decode(&sealed.ciphertext).into_vec().unwrap();
        ct[0] ^= 0x01;
        let tampered = SealedSecret {
            nonce: sealed.nonce.clone(),
            ciphertext: bs58::encode(ct).into_string(),
        };
        assert!(matches!(
            open(&key, &tampered, AAD),
            Err(VaultError::Decrypt)
        ));

        let mut nb = bs58::decode(&sealed.nonce).into_vec().unwrap();
        nb[0] ^= 0x01;
        let moved_nonce = SealedSecret {
            nonce: bs58::encode(nb).into_string(),
            ciphertext: sealed.ciphertext.clone(),
        };
        assert!(matches!(
            open(&key, &moved_nonce, AAD),
            Err(VaultError::Decrypt)
        ));
    }

    #[test]
    fn sealing_the_same_plaintext_twice_gives_distinct_ciphertexts() {
        let key = VaultKey::random();
        let a = seal(&key, b"same", AAD).unwrap();
        let b = seal(&key, b"same", AAD).unwrap();
        // A fresh nonce each time is what keeps two seals of one value
        // from being correlated on the wire.
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.ciphertext, b.ciphertext);
        assert_eq!(open(&key, &a, AAD).unwrap(), open(&key, &b, AAD).unwrap());
    }

    #[test]
    fn a_secret_over_the_limit_is_refused_before_sealing() {
        let key = VaultKey::random();
        let big = vec![0u8; MAX_SECRET_BYTES + 1];
        assert!(matches!(
            seal(&key, &big, AAD),
            Err(VaultError::TooLarge(n)) if n == MAX_SECRET_BYTES + 1
        ));
    }

    #[test]
    fn a_key_round_trips_through_base58() {
        let key = VaultKey::random();
        let restored = VaultKey::from_b58(&key.to_b58()).unwrap();
        assert_eq!(key.as_bytes(), restored.as_bytes());
        assert!(matches!(
            VaultKey::from_b58("not a key!"),
            Err(VaultError::BadKey)
        ));
    }

    #[test]
    fn a_debug_of_a_key_does_not_print_it() {
        let key = VaultKey::from_bytes([7u8; KEY_LEN]);
        let shown = format!("{key:?}");
        assert!(!shown.contains(&key.to_b58()));
        assert!(shown.contains("redacted"));
    }

    #[test]
    fn a_vault_signature_binds_the_key_the_path_and_the_body() {
        let owner = LocalIdentity::generate("owner@local");
        let pubkey = bs58::encode(owner.pubkey_bytes()).into_string();
        let path = "/vault/owner-key/secret/deploy";
        let body = br#"{"nonce":"a","ciphertext":"b"}"#;
        let sig = sign_vault(&owner, path, body, 1_000).unwrap();
        verify_vault(&pubkey, path, body, 1_000, &sig, 2_000).expect("verifies in-window");

        // A different signer, path, or body must not verify.
        let other = LocalIdentity::generate("other@local");
        let other_pk = bs58::encode(other.pubkey_bytes()).into_string();
        assert!(verify_vault(&other_pk, path, body, 1_000, &sig, 2_000).is_err());
        assert!(verify_vault(
            &pubkey,
            "/vault/owner-key/secret/other",
            body,
            1_000,
            &sig,
            2_000
        )
        .is_err());
        assert!(verify_vault(&pubkey, path, b"substituted", 1_000, &sig, 2_000).is_err());
    }

    #[test]
    fn a_vault_signature_outside_the_window_is_rejected() {
        let owner = LocalIdentity::generate("owner@local");
        let pubkey = bs58::encode(owner.pubkey_bytes()).into_string();
        let sig = sign_vault(&owner, "/vault/k/secrets", &[], 1_000).unwrap();
        let err = verify_vault(
            &pubkey,
            "/vault/k/secrets",
            &[],
            1_000,
            &sig,
            1_000 + VAULT_MAX_SKEW_MS + 1,
        )
        .unwrap_err();
        assert!(err.to_string().contains("window"), "got: {err}");
    }
}
