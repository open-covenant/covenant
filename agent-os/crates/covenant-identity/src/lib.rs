//! Local ed25519 identity for Covenant.
//!
//! [`LocalIdentity`] is an ed25519 keypair plus a `name@host` display
//! string. The keypair is persisted as the raw 32-byte seed at
//! `$COVENANT_HOME/identity/local.key` with `0o600` permissions, and
//! the same key is used to sign on-chain settlement transactions —
//! there is no second keypair system.
//!
//! Verification helpers [`verify_with_pubkey`] and
//! [`verifying_key_from_bytes`] cover the read side without forcing
//! callers to depend on `ed25519-dalek` directly.

#![deny(unsafe_code)]

use covenant_types::AgentId;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::RngCore;
use std::path::{Path, PathBuf};
use tracing::info;

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("identity file at {path} has wrong size: expected 32 bytes, got {got}")]
    BadSize { path: PathBuf, got: usize },
    #[error("ed25519: {0}")]
    Crypto(#[from] ed25519_dalek::SignatureError),
}

pub struct LocalIdentity {
    display: String,
    signing_key: SigningKey,
}

impl LocalIdentity {
    /// Generate a fresh identity with the given display string.
    pub fn generate(display: impl Into<String>) -> Self {
        let mut seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut seed);
        let signing_key = SigningKey::from_bytes(&seed);
        Self {
            display: display.into(),
            signing_key,
        }
    }

    pub fn display(&self) -> &str {
        &self.display
    }

    pub fn pubkey_bytes(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }

    /// Borrow the underlying `SigningKey`. Used by `covenant-permissions`
    /// to sign capability tokens. Not published in the wire format.
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }

    pub fn agent_id(&self) -> AgentId {
        AgentId {
            display: self.display.clone(),
            pubkey: self.pubkey_bytes(),
        }
    }

    pub fn sign(&self, message: &[u8]) -> Signature {
        self.signing_key.sign(message)
    }

    /// Load an existing identity from disk; create a new one and persist it
    /// if the file is missing. Default display is `user@local` (kept
    /// generic so logs and commits don't leak the operator's hostname).
    pub fn load_or_create(path: &Path, default_display: &str) -> Result<Self, IdentityError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if path.exists() {
            let bytes = std::fs::read(path)?;
            if bytes.len() != 32 {
                return Err(IdentityError::BadSize {
                    path: path.to_path_buf(),
                    got: bytes.len(),
                });
            }
            let mut seed = [0u8; 32];
            seed.copy_from_slice(&bytes);
            let signing_key = SigningKey::from_bytes(&seed);
            info!(path = %path.display(), "identity loaded");
            return Ok(Self {
                display: default_display.to_string(),
                signing_key,
            });
        }

        let identity = Self::generate(default_display);
        write_with_mode_0600(path, &identity.signing_key.to_bytes())?;
        info!(path = %path.display(), "identity created");
        Ok(identity)
    }
}

#[cfg(unix)]
fn write_with_mode_0600(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    std::io::Write::write_all(&mut f, bytes)
}

#[cfg(not(unix))]
fn write_with_mode_0600(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

pub fn verifying_key_from_bytes(bytes: [u8; 32]) -> Result<VerifyingKey, IdentityError> {
    Ok(VerifyingKey::from_bytes(&bytes)?)
}

pub fn verify_with_pubkey(
    pubkey: [u8; 32],
    message: &[u8],
    signature: &Signature,
) -> Result<(), IdentityError> {
    let vk = verifying_key_from_bytes(pubkey)?;
    vk.verify(message, signature)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn generate_then_sign_and_verify_with_pubkey() {
        let id = LocalIdentity::generate("user@local");
        let msg = b"covenant attestation";
        let sig = id.sign(msg);
        verify_with_pubkey(id.pubkey_bytes(), msg, &sig).unwrap();
    }

    #[test]
    fn verify_rejects_tampered_message() {
        let id = LocalIdentity::generate("user@local");
        let sig = id.sign(b"original");
        assert!(verify_with_pubkey(id.pubkey_bytes(), b"tampered", &sig).is_err());
    }

    #[test]
    fn load_or_create_persists_then_returns_same_pubkey() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("identity").join("local.key");
        let id1 = LocalIdentity::load_or_create(&path, "user@local").unwrap();
        let pub1 = id1.pubkey_bytes();
        let id2 = LocalIdentity::load_or_create(&path, "user@local").unwrap();
        assert_eq!(pub1, id2.pubkey_bytes());
        // File is exactly 32 bytes.
        let written = std::fs::read(&path).unwrap();
        assert_eq!(written.len(), 32);
    }

    #[test]
    fn load_or_create_rejects_wrong_size_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("identity").join("local.key");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"too short").unwrap();
        let r = LocalIdentity::load_or_create(&path, "user@local");
        assert!(matches!(r, Err(IdentityError::BadSize { .. })));
    }

    #[test]
    fn agent_id_round_trips_through_serde() {
        let id = LocalIdentity::generate("research@local");
        let agent_id = id.agent_id();
        let json = serde_json::to_string(&agent_id).unwrap();
        let back: AgentId = serde_json::from_str(&json).unwrap();
        assert_eq!(agent_id, back);
    }
}
