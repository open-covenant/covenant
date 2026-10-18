//! Client-owned encrypted workspace snapshots. The storage service receives only
//! ciphertext; the owner's key stays on the buyer. Each independently sealed
//! chunk binds the owner, workspace, version, floor, total length and position.

use std::collections::BTreeMap;
use std::io::{Read, Write};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use covenant_identity::LocalIdentity;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::sign::{sign_domain, to_canonical_json, verify_domain, ProtocolError};
use crate::vault::VaultKey;

// A single S3 PUT is capped at 5 GiB. Four GiB plus authenticated-chunk
// overhead stays below that limit; larger snapshots need multipart storage.
pub const MAX_WORKSPACE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const MAX_WORKSPACES_PER_OWNER: usize = 16;
pub const CHUNK_BYTES: usize = 1024 * 1024;
pub const WORKSPACE_SIGNED_AT_HEADER: &str = "x-compute-workspace-signed-at-ms";
pub const WORKSPACE_SIGNATURE_HEADER: &str = "x-compute-workspace-signature";
const SIGNATURE_DOMAIN: &[u8] = b"covenant.compute.workspace.request.v1\n";
const SEAL_DOMAIN: &str = "covenant.compute.workspace.snapshot.v1";
const MAGIC: &[u8; 8] = b"COVWS001";
const KEY_DOMAIN: &[u8] = b"covenant.compute.workspace.key.v1";
const HEADER_BYTES: u64 = 44;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceTrustFloor {
    #[default]
    Open,
    Isolated,
    Attested,
    Confidential,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub version: u64,
    pub plaintext_bytes: u64,
    pub ciphertext_bytes: u64,
    pub ciphertext_sha256: String,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceView {
    pub id: Uuid,
    pub owner: String,
    pub name: String,
    pub trust_floor: WorkspaceTrustFloor,
    pub version: u64,
    pub snapshot: Option<WorkspaceSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateWorkspace {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub trust_floor: WorkspaceTrustFloor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceUpload {
    /// A client-generated id makes retries return the same object reservation.
    pub upload_id: Uuid,
    pub expected_version: u64,
    pub plaintext_bytes: u64,
    pub ciphertext_bytes: u64,
    pub ciphertext_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitWorkspace {
    pub upload_id: Uuid,
    pub expected_version: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadWorkspace {
    /// Exact version, never "whatever is latest": a stale restore is refused.
    pub version: u64,
    /// None reads to the owner's machine. Some additionally checks a live lease
    /// belongs to the owner and meets the workspace's minimum trust class.
    pub lease_job_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectGrant {
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceUploadGrant {
    pub upload_id: Uuid,
    pub version: u64,
    pub grant: ObjectGrant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceDownloadGrant {
    pub workspace: WorkspaceView,
    pub grant: ObjectGrant,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceCryptoError {
    #[error("workspace snapshot i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("workspace snapshot is malformed or belongs to another owner, version or trust floor")]
    Invalid,
    #[error("workspace snapshot exceeds the 4 GiB single-upload limit")]
    TooLarge,
}

/// Exact on-disk size, including a final authentication tag for empty files.
pub fn ciphertext_len(plaintext_bytes: u64) -> Result<u64, WorkspaceCryptoError> {
    if plaintext_bytes > MAX_WORKSPACE_BYTES {
        return Err(WorkspaceCryptoError::TooLarge);
    }
    let chunks = plaintext_bytes.div_ceil(CHUNK_BYTES as u64).max(1);
    Ok(HEADER_BYTES + plaintext_bytes + chunks * 16)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    digest_hex(&Sha256::digest(bytes))
}

fn digest_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Debug, Clone)]
pub struct SnapshotContext<'a> {
    pub owner: &'a str,
    pub workspace_id: Uuid,
    pub version: u64,
    pub trust_floor: WorkspaceTrustFloor,
    pub plaintext_bytes: u64,
}

impl SnapshotContext<'_> {
    fn aad(&self, chunk: u64) -> Vec<u8> {
        // Arrays and typed integers eliminate ambiguous concatenations. Every
        // chunk binds the whole-file size, so truncation cannot yield a prefix.
        serde_json::to_vec(&(
            SEAL_DOMAIN,
            self.owner,
            self.workspace_id,
            self.version,
            self.trust_floor,
            self.plaintext_bytes,
            CHUNK_BYTES,
            chunk,
        ))
        .expect("snapshot AAD contains only serializable primitives")
    }
}

// A workspace key is purpose-separated from the caller's vault master key.
// HKDF also binds the owner, workspace id and immutable trust floor; versions
// are bound by each authenticated chunk rather than deriving a new master key.
fn workspace_key(key: &VaultKey, context: &SnapshotContext<'_>) -> zeroize::Zeroizing<[u8; 32]> {
    let info = serde_json::to_vec(&(context.owner, context.workspace_id, context.trust_floor))
        .expect("workspace key context is serializable");
    let hkdf = hkdf::Hkdf::<sha2_10::Sha256>::new(Some(KEY_DOMAIN), key.as_bytes());
    let mut derived = zeroize::Zeroizing::new([0u8; 32]);
    hkdf.expand(&info, derived.as_mut())
        .expect("32 byte HKDF output");
    derived
}

/// Encrypts a bounded-memory stream. The caller must persist the key separately.
/// Returns the SHA-256 S3 must verify over the ciphertext PUT.
pub fn seal_snapshot(
    key: &VaultKey,
    context: &SnapshotContext<'_>,
    mut input: impl Read,
    mut output: impl Write,
) -> Result<String, WorkspaceCryptoError> {
    ciphertext_len(context.plaintext_bytes)?;
    if context.version == 0 {
        return Err(WorkspaceCryptoError::Invalid);
    }
    let derived = workspace_key(key, context);
    let cipher = XChaCha20Poly1305::new_from_slice(derived.as_ref()).expect("32 byte key");
    let mut nonce = [0u8; 24];
    rand::rng().fill_bytes(&mut nonce);
    let mut header = Vec::with_capacity(HEADER_BYTES as usize);
    header.extend_from_slice(MAGIC);
    header.extend_from_slice(&nonce);
    header.extend_from_slice(&context.plaintext_bytes.to_be_bytes());
    header.extend_from_slice(&(CHUNK_BYTES as u32).to_be_bytes());
    let mut hash = Sha256::new();
    output.write_all(&header)?;
    hash.update(&header);
    let mut remaining = context.plaintext_bytes;
    let mut buffer = zeroize::Zeroizing::new(vec![0u8; CHUNK_BYTES]);
    for chunk in 0..context.plaintext_bytes.div_ceil(CHUNK_BYTES as u64).max(1) {
        let size = remaining.min(CHUNK_BYTES as u64) as usize;
        input.read_exact(&mut buffer[..size])?;
        let mut chunk_nonce = nonce;
        for (n, counter) in chunk_nonce[16..].iter_mut().zip(chunk.to_be_bytes()) {
            *n ^= counter;
        }
        let encrypted = cipher
            .encrypt(
                XNonce::from_slice(&chunk_nonce),
                Payload {
                    msg: &buffer[..size],
                    aad: &context.aad(chunk),
                },
            )
            .map_err(|_| WorkspaceCryptoError::Invalid)?;
        output.write_all(&encrypted)?;
        hash.update(&encrypted);
        remaining -= size as u64;
    }
    let mut extra = [0u8; 1];
    if input.read(&mut extra)? != 0 {
        return Err(WorkspaceCryptoError::Invalid);
    }
    Ok(digest_hex(&hash.finalize()))
}

/// Opens exactly the requested version. Write to a private temporary file and
/// publish it only after this function succeeds: later chunks may fail.
pub fn open_snapshot(
    key: &VaultKey,
    context: &SnapshotContext<'_>,
    expected_digest: &str,
    mut input: impl Read,
    mut output: impl Write,
) -> Result<(), WorkspaceCryptoError> {
    ciphertext_len(context.plaintext_bytes)?;
    if context.version == 0 || !valid_digest(expected_digest) {
        return Err(WorkspaceCryptoError::Invalid);
    }
    let mut header = [0u8; HEADER_BYTES as usize];
    input.read_exact(&mut header)?;
    if &header[..8] != MAGIC
        || header[32..40] != context.plaintext_bytes.to_be_bytes()
        || header[40..44] != (CHUNK_BYTES as u32).to_be_bytes()
    {
        return Err(WorkspaceCryptoError::Invalid);
    }
    let nonce: [u8; 24] = header[8..32].try_into().expect("fixed header");
    let derived = workspace_key(key, context);
    let cipher = XChaCha20Poly1305::new_from_slice(derived.as_ref()).expect("32 byte key");
    let mut hash = Sha256::new();
    hash.update(header);
    let mut remaining = context.plaintext_bytes;
    let mut buffer = vec![0u8; CHUNK_BYTES + 16];
    for chunk in 0..context.plaintext_bytes.div_ceil(CHUNK_BYTES as u64).max(1) {
        let size = remaining.min(CHUNK_BYTES as u64) as usize;
        input.read_exact(&mut buffer[..size + 16])?;
        hash.update(&buffer[..size + 16]);
        let mut chunk_nonce = nonce;
        for (n, counter) in chunk_nonce[16..].iter_mut().zip(chunk.to_be_bytes()) {
            *n ^= counter;
        }
        let plaintext = zeroize::Zeroizing::new(
            cipher
                .decrypt(
                    XNonce::from_slice(&chunk_nonce),
                    Payload {
                        msg: &buffer[..size + 16],
                        aad: &context.aad(chunk),
                    },
                )
                .map_err(|_| WorkspaceCryptoError::Invalid)?,
        );
        output.write_all(&plaintext)?;
        remaining -= size as u64;
    }
    let mut extra = [0u8; 1];
    if input.read(&mut extra)? != 0 || digest_hex(&hash.finalize()) != expected_digest {
        return Err(WorkspaceCryptoError::Invalid);
    }
    Ok(())
}

#[derive(Serialize)]
struct WorkspaceClaim<'a> {
    method: &'a str,
    path: &'a str,
    body_sha256: String,
    signed_at_ms: u64,
}

pub fn sign_workspace(
    identity: &LocalIdentity,
    method: &str,
    path: &str,
    body: &[u8],
    signed_at_ms: u64,
) -> Result<String, ProtocolError> {
    let payload = to_canonical_json(&WorkspaceClaim {
        method,
        path,
        body_sha256: sha256_hex(body),
        signed_at_ms,
    })?;
    Ok(sign_domain(identity, SIGNATURE_DOMAIN, &payload).0)
}

pub fn verify_workspace(
    owner: &str,
    method: &str,
    path: &str,
    body: &[u8],
    signed_at_ms: u64,
    signature: &str,
    now_ms: u64,
) -> Result<(), ProtocolError> {
    if now_ms.abs_diff(signed_at_ms) > 120_000 {
        return Err(ProtocolError::Invalid(
            "workspace request signature expired".into(),
        ));
    }
    let payload = to_canonical_json(&WorkspaceClaim {
        method,
        path,
        body_sha256: sha256_hex(body),
        signed_at_ms,
    })?;
    verify_domain(SIGNATURE_DOMAIN, &payload, signature, owner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_keys_use_hkdf_domain_and_identity_separation() {
        let key = VaultKey::from_bytes([7; 32]);
        let context = SnapshotContext {
            owner: "owner",
            workspace_id: Uuid::nil(),
            version: 1,
            trust_floor: WorkspaceTrustFloor::Open,
            plaintext_bytes: 0,
        };
        let derived = workspace_key(&key, &context);
        assert_eq!(
            digest_hex(derived.as_ref()),
            "2be70974c0b9d2d4d111d054570d27cae7cec041576870976ee862b3f73d9802"
        );
        assert_ne!(derived.as_ref(), key.as_bytes());
        assert_ne!(
            derived.as_ref(),
            workspace_key(
                &key,
                &SnapshotContext {
                    owner: "other",
                    ..context.clone()
                }
            )
            .as_ref()
        );
        assert_ne!(
            derived.as_ref(),
            workspace_key(
                &key,
                &SnapshotContext {
                    trust_floor: WorkspaceTrustFloor::Attested,
                    ..context.clone()
                }
            )
            .as_ref()
        );
    }

    #[test]
    fn chunked_snapshot_authenticates_context_order_length_and_bytes() {
        let key = VaultKey::random();
        let context = SnapshotContext {
            owner: "owner",
            workspace_id: Uuid::new_v4(),
            version: 1,
            trust_floor: WorkspaceTrustFloor::Open,
            plaintext_bytes: CHUNK_BYTES as u64 + 19,
        };
        let plain = vec![7u8; context.plaintext_bytes as usize];
        let mut encrypted = Vec::new();
        let hash = seal_snapshot(&key, &context, plain.as_slice(), &mut encrypted).unwrap();
        assert_eq!(
            encrypted.len() as u64,
            ciphertext_len(context.plaintext_bytes).unwrap()
        );
        let mut decoded = Vec::new();
        open_snapshot(&key, &context, &hash, encrypted.as_slice(), &mut decoded).unwrap();
        assert_eq!(decoded, plain);
        for bad in [
            SnapshotContext {
                owner: "other",
                ..context.clone()
            },
            SnapshotContext {
                version: 2,
                ..context.clone()
            },
            SnapshotContext {
                trust_floor: WorkspaceTrustFloor::Confidential,
                ..context.clone()
            },
            SnapshotContext {
                workspace_id: Uuid::new_v4(),
                ..context.clone()
            },
        ] {
            assert!(open_snapshot(&key, &bad, &hash, encrypted.as_slice(), Vec::new()).is_err());
        }
        let mut tampered = encrypted.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(open_snapshot(&key, &context, &hash, tampered.as_slice(), Vec::new()).is_err());
        assert!(open_snapshot(
            &key,
            &context,
            &hash,
            &encrypted[..encrypted.len() - 1],
            Vec::new()
        )
        .is_err());
        assert!(open_snapshot(
            &VaultKey::random(),
            &context,
            &hash,
            encrypted.as_slice(),
            Vec::new()
        )
        .is_err());
    }

    #[test]
    fn empty_snapshot_is_authenticated_and_each_seal_is_fresh() {
        let key = VaultKey::random();
        let context = SnapshotContext {
            owner: "o",
            workspace_id: Uuid::new_v4(),
            version: 1,
            trust_floor: WorkspaceTrustFloor::Open,
            plaintext_bytes: 0,
        };
        let mut first = Vec::new();
        let mut second = Vec::new();
        let hash = seal_snapshot(&key, &context, &[][..], &mut first).unwrap();
        seal_snapshot(&key, &context, &[][..], &mut second).unwrap();
        assert_ne!(first, second);
        assert_eq!(first.len(), 60);
        open_snapshot(&key, &context, &hash, first.as_slice(), Vec::new()).unwrap();
        assert!(ciphertext_len(MAX_WORKSPACE_BYTES + 1).is_err());
    }

    #[test]
    fn signatures_cannot_cross_owners_routes_methods_or_bodies() {
        let owner = LocalIdentity::generate("owner@local");
        let pk = owner.agent_id().pubkey_base58();
        let sig = sign_workspace(&owner, "POST", "/workspaces/a", b"one", 1000).unwrap();
        verify_workspace(&pk, "POST", "/workspaces/a", b"one", 1000, &sig, 2000).unwrap();
        for (method, path, body, now) in [
            ("DELETE", "/workspaces/a", b"one".as_slice(), 2000),
            ("POST", "/workspaces/b", b"one".as_slice(), 2000),
            ("POST", "/workspaces/a", b"two".as_slice(), 2000),
            ("POST", "/workspaces/a", b"one".as_slice(), 122000),
        ] {
            assert!(verify_workspace(&pk, method, path, body, 1000, &sig, now).is_err());
        }
    }
}
