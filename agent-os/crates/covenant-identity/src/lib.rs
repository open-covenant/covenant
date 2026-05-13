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
    #[error("identity file at {path} has insecure permissions {mode:#o}; require 0o600")]
    InsecureMode { path: PathBuf, mode: u32 },
    #[error("identity file at {path} is a symlink; refusing to follow")]
    Symlink { path: PathBuf },
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
            set_dir_mode_0700(parent)?;
        }
        if path.exists() {
            require_identity_key_secure(path)?;
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
fn require_identity_key_secure(path: &Path) -> Result<(), IdentityError> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(IdentityError::Symlink {
            path: path.to_path_buf(),
        });
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode != 0o600 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        let after = std::fs::symlink_metadata(path)?.permissions().mode() & 0o777;
        if after != 0o600 {
            return Err(IdentityError::InsecureMode {
                path: path.to_path_buf(),
                mode,
            });
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn require_identity_key_secure(_path: &Path) -> Result<(), IdentityError> {
    Ok(())
}

#[cfg(unix)]
fn set_dir_mode_0700(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_dir_mode_0700(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn write_with_mode_0600(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
    let mut f = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    std::io::Write::write_all(&mut f, bytes)?;
    f.sync_all()
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

    #[cfg(unix)]
    #[test]
    fn load_or_create_pins_key_file_and_parent_dir_mode_invariants() {
        // The crate header (line 4-7) documents that the ed25519 seed
        // at $COVENANT_HOME/identity/local.key is persisted with 0o600
        // permissions and that the same key signs on-chain settlement
        // transactions. The unix path also chmods the parent directory
        // to 0o700 (line 83 set_dir_mode_0700) and auto-repairs an
        // existing key file from any non-0o600 mode back to 0o600
        // before reading (line 121-129 require_identity_key_secure).
        //
        // None of these three filesystem-mode invariants are observed
        // by existing tests: load_or_create_persists_then_returns_same_pubkey
        // asserts the file is 32 bytes and round-trips the pubkey;
        // load_or_create_rejects_wrong_size_file asserts BadSize on a
        // short file; but no test reads st_mode on the produced key
        // file or its parent directory, and no test exercises the
        // auto-repair branch from 0o644 → 0o600. A refactor that
        // dropped .mode(0o600) on the OpenOptionsExt builder (line 159),
        // dropped set_dir_mode_0700 (line 142), or flipped the
        // auto-repair branch to reject-on-bad-mode would silently
        // degrade the security boundary on the operator's identity
        // key with no parse-time or compile-time signal — the doc
        // comment in the crate header would still claim the boundary
        // exists.
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let path = dir.path().join("identity").join("local.key");

        let _id = LocalIdentity::load_or_create(&path, "user@local").unwrap();

        let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            file_mode, 0o600,
            "load_or_create must write the identity key file at mode \
             0o600 — the ed25519 seed signs every settlement \
             transaction and a refactor that dropped the .mode(0o600) \
             call on the OpenOptionsExt builder (line 159) would let \
             the operator's umask determine the file mode (commonly \
             0o022 yielding 0o644) and the seed would sit world-\
             readable on disk with the only signal being the crate's \
             doc-comment claim, not a runtime check on first write; \
             got mode {:#o}",
            file_mode,
        );

        let parent_mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            parent_mode, 0o700,
            "load_or_create must chmod the parent identity directory \
             to 0o700 — a refactor that dropped set_dir_mode_0700 \
             (line 83/142) or replaced it with default \
             create_dir_all permissions would leave the directory at \
             whatever the operator's umask permits (typically 0o755) \
             and ancillary files written by future agents (cached \
             attestations, identity provenance scratch, per-peer \
             rotation state) would become world-readable even if \
             each individual file is written 0o600; directory-listing \
             leaks become silent; got mode {:#o}",
            parent_mode,
        );

        // Auto-repair branch: pre-create a valid 32-byte seed at
        // 0o644 (the failure-mode dropped-mode-on-write would produce),
        // then call load_or_create on it. require_identity_key_secure
        // (line 121-129) must observe the wrong mode, set_permissions
        // to 0o600, and proceed — not reject with InsecureMode. The
        // returned identity must sign and verify correctly, proving
        // the repair didn't corrupt the seed.
        let repair_dir = tempdir().unwrap();
        let repair_path = repair_dir.path().join("identity").join("local.key");
        std::fs::create_dir_all(repair_path.parent().unwrap()).unwrap();
        let seed = [7u8; 32];
        std::fs::write(&repair_path, seed).unwrap();
        std::fs::set_permissions(&repair_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let pre_mode = std::fs::metadata(&repair_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            pre_mode, 0o644,
            "test precondition: the pre-created key file must be at \
             0o644 to exercise the repair branch; got {:#o}",
            pre_mode,
        );

        let repaired = LocalIdentity::load_or_create(&repair_path, "user@local").expect(
            "require_identity_key_secure must auto-repair a 0o644 key \
             file in place rather than reject with InsecureMode; a \
             refactor that flipped the branch to fail-closed would \
             break every operator who restored from a backup that \
             didn't preserve mode bits or who migrated an older \
             agent's identity file forward, with no migration \
             guidance — the InsecureMode error message claims \
             'require 0o600' but the code actually attempts repair \
             first, and that contract must stay",
        );

        let post_mode = std::fs::metadata(&repair_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            post_mode, 0o600,
            "require_identity_key_secure must leave the file at \
             0o600 after auto-repair — a refactor that skipped the \
             re-stat-and-verify branch (line 123-129) would let a \
             chmod failure silently pass through and the file would \
             stay at 0o644 with the doc-comment claim still pointing \
             at 0o600; got post-repair mode {:#o}",
            post_mode,
        );

        let msg = b"covenant identity repair pin";
        let sig = repaired.sign(msg);
        verify_with_pubkey(repaired.pubkey_bytes(), msg, &sig).expect(
            "the repaired identity's seed must sign and verify — \
             pinning that the auto-repair branch did not corrupt the \
             on-disk 32-byte seed in the process of fixing the mode \
             bits; a refactor that re-wrote the file in the repair \
             path (rather than only chmod) could silently shuffle \
             bytes and the seed would round-trip wrong",
        );
    }
}
