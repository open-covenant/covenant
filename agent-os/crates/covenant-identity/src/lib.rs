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
        rand::rng().fill_bytes(&mut seed);
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
    fn verify_with_pubkey_rejects_tampered_signature_and_wrong_pubkey() {
        // verify_with_pubkey (line 174-182) is the public verification
        // entry point used by covenant_permissions::verify, audit-log
        // integrity checks, and any future settlement-attestation
        // flow. Three regression classes can break it: tampered
        // message (already pinned by verify_rejects_tampered_message
        // above), tampered signature (one bit flipped in a valid
        // signature must still fail), and wrong pubkey (a signature
        // produced by key A must fail to verify against key B's
        // pubkey). This pin closes the second and third arms so the
        // three-arm regression family is fully covered.
        //
        // A refactor that swapped vk.verify(message, signature) for a
        // no-op success branch (e.g., during a debugging session)
        // would pass the existing tampered-message pin because the
        // no-op accepts everything. The tampered-signature arm
        // catches that regression. A refactor that ignored the
        // supplied pubkey and looked up a process-wide trust-store
        // key would pass both tampered-message and tampered-signature
        // pins; the wrong-pubkey arm catches that regression.
        let alice = LocalIdentity::generate("alice@local");
        let bob = LocalIdentity::generate("bob@local");
        let message = b"covenant verification pin";

        let sig = alice.sign(message);
        verify_with_pubkey(alice.pubkey_bytes(), message, &sig).expect(
            "happy path: a fresh signature must verify against the \
             producing key's pubkey — pinning the baseline against \
             which the tamper and wrong-pubkey arms diverge",
        );

        let mut sig_bytes = sig.to_bytes();
        sig_bytes[32] ^= 0x01;
        let tampered_sig = Signature::from_bytes(&sig_bytes);
        assert!(
            verify_with_pubkey(alice.pubkey_bytes(), message, &tampered_sig).is_err(),
            "tampered signature must fail to verify: a refactor that \
             swapped vk.verify(message, signature) for a no-op success \
             branch (e.g., during a debugging session that got \
             accidentally committed, or a short-circuit that bypassed \
             the ed25519 verify call) would silently accept any \
             signature against the original message + pubkey — \
             pinning this arm catches the no-op regression that the \
             existing tampered-message pin lets through (because a \
             no-op also accepts tampered messages but the wrong-\
             pubkey arm below catches the ignored-pubkey regression \
             which the no-op also passes)",
        );

        assert!(
            verify_with_pubkey(bob.pubkey_bytes(), message, &sig).is_err(),
            "wrong-pubkey verification must fail: alice's signature \
             must not verify against bob's pubkey — a refactor that \
             ignored the supplied pubkey and looked up a process-\
             wide trust-store key (or that accidentally used the \
             signing-side cached pubkey) would let an attacker \
             substitute their own granted_by pubkey on a capability \
             and still pass verify, opening every authorization \
             boundary",
        );
    }

    #[test]
    fn sign_pins_rfc8032_determinism_message_dependence_and_key_dependence() {
        // LocalIdentity::sign (line 73-75) delegates to
        // ed25519_dalek::SigningKey::sign which implements RFC 8032
        // EdDSA — signatures are deterministic: signing the same
        // message with the same key produces byte-identical bytes
        // every call. Existing sign-related tests
        // (generate_then_sign_and_verify_with_pubkey,
        // verify_rejects_tampered_message,
        // verify_with_pubkey_rejects_tampered_signature_and_wrong_pubkey)
        // call sign() once each and exercise verify, never pinning
        // determinism. Three independent regression classes are
        // unpinned by the verify-side coverage:
        //
        // (1) determinism: a refactor that introduced per-call
        //     entropy (e.g., randomized-nonce variant for
        //     observability disambiguation) would still verify
        //     successfully because RFC-conforming randomized
        //     signatures verify, but every byte-equality check on
        //     persisted capability tokens, every signature-cache
        //     hit-rate metric, and every audit-row signature-
        //     fingerprint comparison would silently regress.
        //
        // (2) message-dependence: a debug stub that returned a
        //     constant signature regardless of message
        //     ('return Signature::from_bytes(&[0u8; 64])') would
        //     satisfy determinism while breaking the security
        //     contract — message-dependence rules out that arm.
        //
        // (3) key-dependence: a refactor that returned a key-
        //     independent signature ('return sha256(message)' or a
        //     module-level captured SigningKey) would satisfy
        //     determinism + message-dependence while letting any
        //     holder of a message produce a valid signature for any
        //     key — alice's signature would be indistinguishable
        //     from bob's, opening every authorization boundary that
        //     keys on signer identity.
        let alice = LocalIdentity::generate("alice@local");
        let bob = LocalIdentity::generate("bob@local");

        let sig1 = alice.sign(b"covenant");
        let sig2 = alice.sign(b"covenant");
        assert_eq!(
            sig1.to_bytes(),
            sig2.to_bytes(),
            "ed25519 signatures are deterministic per RFC 8032 — \
             a refactor that introduced per-call entropy (e.g., \
             randomized-nonce variant or a debug surface that \
             added a counter to disambiguate concurrent signs) \
             would silently break byte-equality of persisted \
             capability tokens and audit-row signature \
             fingerprints, while the verify-side tests would still \
             pass because RFC-conforming randomized signatures \
             verify",
        );

        let sig_y = alice.sign(b"y");
        assert_ne!(
            sig1.to_bytes(),
            sig_y.to_bytes(),
            "signatures must depend on the message — a debug stub \
             that returned a constant signature regardless of \
             message ('return Signature::from_bytes(&[0u8; 64])') \
             would satisfy the determinism arm but every message \
             would carry the same signature, breaking the security \
             contract",
        );

        let bob_sig = bob.sign(b"covenant");
        assert_ne!(
            sig1.to_bytes(),
            bob_sig.to_bytes(),
            "signatures must depend on the signing key — a refactor \
             that returned a key-independent signature ('return \
             sha256(message)' or a module-level captured \
             SigningKey) would satisfy the determinism and \
             message-dependence arms while letting any holder of a \
             message produce a valid signature for any key, \
             opening every authorization boundary that keys on \
             signer identity",
        );
    }

    #[test]
    fn agent_id_pins_display_and_pubkey_composition_from_self() {
        // LocalIdentity::agent_id (line 66-71) composes an AgentId
        // by cloning self.display into display and calling
        // self.pubkey_bytes() into pubkey. The result feeds
        // covenant_audit::AuditEvent.issuer (every audit row's
        // issuer field), covenant_permissions::Capability.granted_by
        // (every capability's signer identity), and covenant_a2a
        // peer-routing identity lookups.
        //
        // agent_id_round_trips_through_serde (line above) verifies
        // serde round-trip but does NOT pin the composition: it
        // never asserts that agent_id().display equals
        // self.display() or that agent_id().pubkey equals
        // self.pubkey_bytes(). A refactor that swapped
        // self.display.clone() for a constant ('user@local',
        // 'system') would silently relabel every audit row, every
        // capability grant, and every a2a peer announcement to one
        // identity. A refactor that swapped self.pubkey_bytes() for
        // self.signing_key().to_bytes() (the seed bytes, NOT the
        // verifying-key bytes) would silently leak the private seed
        // into every AgentId.pubkey field, breaking ed25519
        // signature verification and exposing the signing key in
        // every operator-readable surface.
        let alice = LocalIdentity::generate("alice@local");
        let bob = LocalIdentity::generate("bob@local");

        let alice_agent = alice.agent_id();
        assert_eq!(
            alice_agent.display,
            alice.display(),
            "AgentId.display must equal self.display() byte-for-byte \
             — a refactor that swapped self.display.clone() for a \
             constant string under a 'normalize display fields for \
             cross-environment audit consistency' rationale would \
             silently relabel every audit row's issuer to one \
             identity, breaking the issuer-based incident-response \
             workflow that audit chains exist to support",
        );
        assert_eq!(
            alice_agent.pubkey,
            alice.pubkey_bytes(),
            "AgentId.pubkey must equal self.pubkey_bytes() byte-for-\
             byte — a refactor that swapped self.pubkey_bytes() for \
             self.signing_key().to_bytes() (the seed, NOT the \
             verifying-key bytes) under a 'use the same byte source \
             for both signing and identity' rationale would silently \
             leak the private seed into every AgentId.pubkey field, \
             breaking ed25519 signature verification and exposing \
             the signing key in every persisted audit row",
        );

        let bob_agent = bob.agent_id();
        assert_ne!(
            alice_agent, bob_agent,
            "two distinct identities must produce distinct AgentIds \
             — a refactor that returned a constant or default \
             AgentId (e.g., 'AgentId::default()' as a partial mock, \
             or 'AgentId {{ display: \"\".into(), pubkey: [0u8; 32] }}' \
             as a placeholder) would satisfy the field-passthrough \
             arms when both identities happened to share state, but \
             this distinctness arm catches the constant-AgentId \
             regression class that would otherwise collapse every \
             audit row to one issuer with a zero pubkey",
        );
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
    fn identity_error_display_messages_pin_three_field_bearing_variant_format_strings() {
        // IdentityError has five variants. Two wrap external errors via
        // #[from] (Io, Crypto); the three field-bearing variants emit
        // security-relevant operator-facing format strings that no
        // existing test inspects via Display.
        // load_or_create_rejects_wrong_size_file asserts BadSize via
        // `matches!` and ignores Display;
        // load_or_create_rejects_symlink_at_identity_key_path asserts
        // the path field of the Symlink variant but not the message.
        // The literal '32 bytes', the {mode:#o} octal formatter, and
        // the 'refusing to follow' hint are all load-bearing for
        // operator triage of identity-file regressions.

        let bad_size = IdentityError::BadSize {
            path: PathBuf::from("/tmp/key"),
            got: 7,
        };
        let bad_size_message = format!("{bad_size}");
        assert!(
            bad_size_message.contains("/tmp/key"),
            "BadSize must surface the path: {bad_size_message}"
        );
        assert!(
            bad_size_message.contains("wrong size"),
            "BadSize must keep the 'wrong size' phrase — distinguishes \
             this variant from other size-related failures (e.g., the \
             ed25519 SignatureError wrapped in Crypto): {bad_size_message}"
        );
        assert!(
            bad_size_message.contains("expected 32 bytes"),
            "BadSize must surface the literal 'expected 32 bytes' — \
             the ed25519 seed length is invariant; operators reading \
             a malformed-key error need to know the EXPECTED size to \
             either repair the file or rotate the key. A typo that \
             dropped '32 bytes' under a 'less verbose error messages' \
             rationale would strip the operator's only sizing hint: \
             {bad_size_message}"
        );
        assert!(
            bad_size_message.contains("got 7"),
            "BadSize must surface the actual size — pins the {{got}} \
             binding so a refactor that dropped the field from the \
             format string surfaces here: {bad_size_message}"
        );

        let insecure = IdentityError::InsecureMode {
            path: PathBuf::from("/tmp/key"),
            mode: 0o644,
        };
        let insecure_message = format!("{insecure}");
        assert!(
            insecure_message.contains("/tmp/key"),
            "InsecureMode must surface the path: {insecure_message}"
        );
        assert!(
            insecure_message.contains("insecure permissions"),
            "InsecureMode must keep the 'insecure permissions' phrase: \
             {insecure_message}"
        );
        assert!(
            insecure_message.contains("0o644"),
            "InsecureMode must emit the mode in OCTAL via {{mode:#o}} \
             — a refactor that dropped the '#' flag would emit \
             decimal ('420' for 0o644). The '0o' prefix is what makes \
             the mode bits visually scannable against the documented \
             0o600 baseline; without it, operators reading the decimal \
             form cannot match the value against /etc/permissions \
             tables: {insecure_message}"
        );
        assert!(
            insecure_message.contains("require 0o600"),
            "InsecureMode must surface the literal 'require 0o600' — \
             the daemon's expected baseline lives only in this error \
             string. A refactor that templated the baseline (e.g., \
             '{{REQUIRED:#o}}') without updating the constant would \
             silently let the baseline drift; the literal anchors the \
             0o600 expectation: {insecure_message}"
        );

        let symlink = IdentityError::Symlink {
            path: PathBuf::from("/tmp/key"),
        };
        let symlink_message = format!("{symlink}");
        assert!(
            symlink_message.contains("/tmp/key"),
            "Symlink must surface the path: {symlink_message}"
        );
        assert!(
            symlink_message.contains("is a symlink"),
            "Symlink must keep the 'is a symlink' phrase: \
             {symlink_message}"
        );
        assert!(
            symlink_message.contains("refusing to follow"),
            "Symlink must keep the 'refusing to follow' hint — the \
             security-relevant diagnostic that distinguishes 'we won't \
             resolve' (intentional policy to block attacker-planted \
             symlinks at $COVENANT_HOME/identity/local.key) from 'we \
             couldn't resolve' (filesystem error). A rewrite to \
             'identity file at {{path}} is a symlink' under a 'less \
             verbose' pass would strip the entire policy signal: \
             {symlink_message}"
        );
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
    fn load_or_create_rejects_symlink_at_identity_key_path() {
        // require_identity_key_secure (line 112-132, unix path) uses
        // std::fs::symlink_metadata to read the key file's metadata
        // without following symlinks, then explicitly rejects any
        // path whose file type is a symlink with IdentityError::Symlink
        // (line 115-118). This is the security boundary that catches
        // an attacker-planted symlink at $COVENANT_HOME/identity/local.key
        // pointing at an attacker-controlled file before the daemon
        // can sign settlement transactions with the contents of that
        // file. The Symlink variant of IdentityError is defined
        // (line 29-30) and the rejection branch is structurally
        // present, but no existing test exercises it:
        // load_or_create_persists_then_returns_same_pubkey and
        // load_or_create_rejects_wrong_size_file cover the regular-
        // file path, and the recently-added
        // load_or_create_pins_key_file_and_parent_dir_mode_invariants
        // covers mode-bit invariants but not symlink rejection. A
        // refactor that switched from symlink_metadata to metadata
        // (which follows symlinks transparently — is_symlink() on the
        // followed target always returns false even when the original
        // path is a symlink) or dropped the file_type().is_symlink()
        // branch entirely would silently allow a malicious symlink to
        // redirect the seed read with no parse-time signal.
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let target = dir.path().join("real-key");
        let seed = [9u8; 32];
        std::fs::write(&target, seed).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();

        let identity_dir = dir.path().join("identity");
        std::fs::create_dir_all(&identity_dir).unwrap();
        let symlink_path = identity_dir.join("local.key");
        std::os::unix::fs::symlink(&target, &symlink_path).unwrap();

        let result = LocalIdentity::load_or_create(&symlink_path, "user@local");
        match result {
            Err(IdentityError::Symlink { path }) => {
                assert_eq!(
                    path, symlink_path,
                    "IdentityError::Symlink must surface the symlink \
                     path (the one passed to load_or_create), not the \
                     target path that the symlink resolves to; \
                     operators reading the error message need to know \
                     which entry in $COVENANT_HOME/identity/ to \
                     remove, and surfacing the resolved target would \
                     leak attacker-controlled path content into log \
                     output",
                );
            }
            Err(other) => panic!(
                "load_or_create on a symlink path must return \
                 Err(IdentityError::Symlink); a refactor that swapped \
                 symlink_metadata for metadata (which follows \
                 symlinks) or dropped the file_type().is_symlink() \
                 branch would silently widen the trust boundary to \
                 include attacker-controlled symlink targets, and the \
                 daemon would sign settlement transactions under the \
                 contents of whatever file the symlink resolved to; \
                 got Err({:?})",
                other,
            ),
            Ok(_) => panic!(
                "load_or_create on a symlink path must return \
                 Err(IdentityError::Symlink); got Ok — the symlink \
                 was silently followed and an attacker-planted \
                 symlink would let the daemon sign transactions \
                 under the contents of the resolved target",
            ),
        }
    }

    #[cfg(unix)]
    #[test]
    fn load_or_create_pins_key_file_and_parent_dir_mode_invariants() {
        // The crate header documents that the ed25519 seed at
        // $COVENANT_HOME/identity/local.key is persisted with 0o600
        // permissions and that the same key signs on-chain settlement
        // transactions. The unix path also chmods the parent directory
        // to 0o700 via set_dir_mode_0700 and auto-repairs an existing
        // key file from any non-0o600 mode back to 0o600 before reading
        // via require_identity_key_secure.
        //
        // None of these three filesystem-mode invariants are observed
        // by existing tests: load_or_create_persists_then_returns_same_pubkey
        // asserts the file is 32 bytes and round-trips the pubkey;
        // load_or_create_rejects_wrong_size_file asserts BadSize on a
        // short file; but no test reads st_mode on the produced key
        // file or its parent directory, and no test exercises the
        // auto-repair branch from 0o644 → 0o600. A refactor that
        // dropped .mode(0o600) on the OpenOptionsExt builder, dropped
        // set_dir_mode_0700, or flipped the auto-repair branch to
        // reject-on-bad-mode would silently degrade the security
        // boundary on the operator's identity key with no parse-time
        // or compile-time signal — the doc comment in the crate header
        // would still claim the boundary exists.
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
             call on the OpenOptionsExt builder would let the \
             operator's umask determine the file mode (commonly 0o022 \
             yielding 0o644) and the seed would sit world-readable on \
             disk with the only signal being the crate's doc-comment \
             claim, not a runtime check on first write; got mode {:#o}",
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

    #[test]
    fn identity_error_io_and_crypto_display_messages_pin_prefix_and_external_source_display_delegation(
    ) {
        let io_err = IdentityError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "identity.key missing",
        ));
        let io_message = format!("{io_err}");
        assert!(
            io_message.starts_with("io: "),
            "IdentityError::Io must surface the literal 'io: ' bootstrap-stage prefix so audit-log filters can distinguish daemon-identity-file disk faults from crypto/key faults and from the three security-relevant field-bearing surfaces (dropped-prefix regression class): {io_message}"
        );
        assert!(
            io_message.contains("identity.key missing"),
            "IdentityError::Io must surface the inner std::io::Error Display rendering after the colon ({{0}}, not {{0:?}}) (Debug-vs-Display formatting regression class on the {{0}} interpolation): {io_message}"
        );
        assert!(
            !io_message.contains("Custom {") && !io_message.contains("Os {"),
            "IdentityError::Io must NOT surface the std::io::Error Debug rendering (Debug-vs-Display formatting regression class on the {{0}} interpolation): {io_message}"
        );

        // Produce a SignatureError via verify_strict failure — the
        // construction used in covenant-permissions for the same reason
        // (VerifyingKey::from_bytes is unreliable as a fixture because
        // most byte patterns happen to decode to a valid Edwards point).
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let signature = Signer::sign(&signing_key, b"original message");
        let verifying_key = signing_key.verifying_key();
        let crypto_source = verifying_key
            .verify_strict(b"different message", &signature)
            .expect_err("verify_strict must fail on mismatched message");
        let crypto_err = IdentityError::Crypto(crypto_source);
        let crypto_message = format!("{crypto_err}");
        assert!(
            crypto_message.starts_with("ed25519: "),
            "IdentityError::Crypto must surface the literal 'ed25519: ' bootstrap-stage prefix so audit-log filters can distinguish identity crypto faults (malformed seed/keypair, signature verification failure) from disk faults and from the three security-relevant field-bearing surfaces (dropped-prefix regression class): {crypto_message}"
        );
        assert!(
            crypto_message.len() > "ed25519: ".len(),
            "IdentityError::Crypto must surface the inner ed25519_dalek::SignatureError Display rendering after the colon ({{0}}, not {{0:?}}); a refactor that dropped the {{0}} would leave only the prefix (dropped-source-rendering regression class on the {{0}} interpolation): {crypto_message}"
        );

        assert_ne!(
            io_message, crypto_message,
            "IdentityError::Io and IdentityError::Crypto Display must not converge (prefix-convergence regression class): io={io_message} crypto={crypto_message}"
        );
        assert!(
            !io_message.starts_with("ed25519:") && !crypto_message.starts_with("io:"),
            "IdentityError::Io must not start with 'ed25519:' and IdentityError::Crypto must not start with 'io:' (sibling-prefix-swap regression class): io={io_message} crypto={crypto_message}"
        );

        let security_surfaces = [
            "has wrong size: expected 32 bytes",
            "has insecure permissions",
            "is a symlink; refusing to follow",
        ];
        for surface in security_surfaces {
            assert!(
                !io_message.contains(surface),
                "IdentityError::Io must not converge with the security-relevant field-bearing surface '{surface}' (security-surface-convergence regression class): {io_message}"
            );
            assert!(
                !crypto_message.contains(surface),
                "IdentityError::Crypto must not converge with the security-relevant field-bearing surface '{surface}' (security-surface-convergence regression class): {crypto_message}"
            );
        }
    }

    #[test]
    fn identity_error_io_source_delegation_pin_returns_inner_std_io_error_via_std_error_source() {
        use std::error::Error;

        let inner = std::io::Error::new(std::io::ErrorKind::NotFound, "identity.ed25519 missing");
        let expected_display = format!("{inner}");
        let err = IdentityError::Io(inner);
        let source = err.source().expect(
            "covenant_identity::IdentityError::Io must surface the inner std::io::Error via std::error::Error::source so identity-bootstrap retry-policy classifiers can walk the error chain and downcast source() to std::io::Error to extract io::ErrorKind for distinct decisions on identity-key IO (NotFound triggers first-run identity generation, PermissionDenied escalates as a security-sensitive incident on the on-disk private key, Interrupted retries immediately); a refactor that converted the variant from #[from] to a hand-written Error impl returning None (under a 'simpler error wrapping' rationale) would silently change source() to return None while leaving Display intact (dropped-source-attribute regression class)",
        );
        assert_eq!(
            format!("{source}"),
            expected_display,
            "covenant_identity::IdentityError::Io source() Display must match a direct format!() of the same std::io::Error verbatim; a refactor that swapped the inner field type to Box<dyn Error + Send + Sync> or any other wrapper would silently break daemon-side downcasts even though the wrapper's Display would continue to flow through {{0}} (concrete-source-type regression class)"
        );
        let kind = source.downcast_ref::<std::io::Error>().map(|e| e.kind());
        assert_eq!(
            kind,
            Some(std::io::ErrorKind::NotFound),
            "covenant_identity::IdentityError::Io source() must downcast_ref to std::io::Error so identity-bootstrap classifiers can distinguish NotFound (cold-start, generate fresh identity) from PermissionDenied (security incident on the on-disk private key); a refactor that wrapped the inner in a project-local newtype (e.g., IdentityIoError(std::io::Error) under a 'tag identity IO failures distinctly from sibling Io variants in other crates' rationale) would silently break downcast_ref::<std::io::Error>() at every downstream callsite that classifies identity-key IO faults (concrete-source-type downcast regression class)"
        );
    }

    #[test]
    fn identity_error_crypto_source_delegation_pin_returns_inner_ed25519_signature_error_via_std_error_source(
    ) {
        use std::error::Error;

        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let signature = Signer::sign(&signing_key, b"original message");
        let verifying_key = signing_key.verifying_key();
        let inner = verifying_key
            .verify_strict(b"different message", &signature)
            .expect_err("verify_strict must fail on mismatched message");
        let expected_display = format!("{inner}");
        let err = IdentityError::Crypto(inner);
        let source = err.source().expect(
            "IdentityError::Crypto must surface the inner ed25519_dalek::SignatureError via std::error::Error::source so daemon-identity bootstrap audit emitters can walk the error chain and downcast source() to ed25519_dalek::SignatureError for distinct triage of 'malformed seed/keypair' (key-decode failure path) vs 'signature verification failed' (cryptographic mismatch path); a refactor that converted the variant from #[from] to a hand-written Error impl returning None (under a 'simpler error wrapping' rationale) would silently change source() to return None while leaving Display intact (dropped-source-attribute regression class)",
        );
        assert_eq!(
            format!("{source}"),
            expected_display,
            "IdentityError::Crypto source() Display must match a direct format!() of the same ed25519_dalek::SignatureError verbatim; a refactor that swapped the inner field type to Box<dyn Error + Send + Sync> or any other wrapper would silently break daemon-identity downcasts even though the wrapper's Display would continue to flow through {{0}} (concrete-source-type regression class)"
        );
        assert!(
            source.downcast_ref::<ed25519_dalek::SignatureError>().is_some(),
            "IdentityError::Crypto source() must downcast_ref to ed25519_dalek::SignatureError so daemon-identity audit emitters can extract the concrete crypto-failure type for triage; a refactor that wrapped the inner in a project-local newtype (e.g., IdentityCryptoError(ed25519_dalek::SignatureError) under a 'distinguish daemon-identity crypto failures from sibling crypto-error sites' rationale) would silently break downcast_ref::<ed25519_dalek::SignatureError>() at every downstream callsite (concrete-source-type downcast regression class)"
        );
    }
}
