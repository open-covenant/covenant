//! Buyer-side vault: seal a secret locally under a key only you hold,
//! store the ciphertext with the coordinator, and fetch and open it
//! later. The coordinator keeps bytes it cannot read, so a stored secret
//! is private to whoever holds the key, not to whoever runs the network.
//!
//! Every call signs the request the same way the signed-read helpers do:
//! the owner's key signs the method and path, and a store also signs the
//! exact ciphertext it writes. The key never leaves this side.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use covenant_compute_protocol::{
    sign_vault, vault_list_path, vault_open, vault_seal, vault_secret_path, vault_signing_path,
    SealedSecret, SecretMeta, VaultKey, VAULT_SIGNATURE_HEADER, VAULT_SIGNED_AT_HEADER,
};
use covenant_identity::LocalIdentity;
use serde::{Deserialize, Serialize};

use crate::{BuyerConfig, BuyerError};

/// Signs `req` for `signing_path` over `body` and sends it. `body` is the
/// bytes the request carries — the sealed ciphertext for a store, empty
/// for a read or delete — and must match what the caller set on `req`.
async fn send_signed(
    req: reqwest::RequestBuilder,
    identity: &LocalIdentity,
    signing_path: &str,
    body: &[u8],
    config: &BuyerConfig,
    action: &'static str,
) -> Result<reqwest::Response, BuyerError> {
    let signed_at_ms = crate::epoch_ms();
    let signature = sign_vault(identity, signing_path, body, signed_at_ms)
        .map_err(|e| BuyerError::Protocol(e.to_string()))?;
    req.header(VAULT_SIGNED_AT_HEADER, signed_at_ms.to_string())
        .header(VAULT_SIGNATURE_HEADER, signature)
        .send()
        .await
        .map_err(|e| BuyerError::unreachable(&config.coordinator_url, action, &e))
}

async fn expect_success(resp: reqwest::Response, action: &str) -> Result<(), BuyerError> {
    if resp.status().is_success() {
        return Ok(());
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    Err(BuyerError::Coordinator(format!(
        "{action} returned {status}: {}",
        covenant_compute_protocol::coordinator_reason(&body)
    )))
}

/// Seals `plaintext` under `key` and stores it at `label`, replacing any
/// secret already there. The coordinator receives only the ciphertext.
pub async fn vault_store(
    http: &reqwest::Client,
    config: &BuyerConfig,
    identity: &LocalIdentity,
    key: &VaultKey,
    label: &str,
    plaintext: &[u8],
) -> Result<(), BuyerError> {
    let owner = identity.agent_id().pubkey_base58();
    let path = vault_secret_path(&owner, label);
    // Bind the seal to this secret's own path, so a store that swapped one
    // owner's ciphertexts between labels is caught when the buyer opens it.
    let sealed = vault_seal(key, plaintext, path.as_bytes())
        .map_err(|e| BuyerError::Protocol(e.to_string()))?;
    let body = serde_json::to_vec(&sealed).map_err(|e| BuyerError::Protocol(e.to_string()))?;
    let base = config.coordinator_url.trim_end_matches('/');
    let req = http.post(format!("{base}{path}")).body(body.clone());
    let signing_path = vault_signing_path("POST", &path);
    let resp = send_signed(
        req,
        identity,
        &signing_path,
        &body,
        config,
        "store this secret",
    )
    .await?;
    expect_success(resp, "vault store").await
}

/// Fetches the secret at `label` and opens it with `key`. The returned
/// bytes are the plaintext the buyer sealed; the coordinator never saw
/// them. A missing secret is a coordinator error, not an empty result.
pub async fn vault_fetch(
    http: &reqwest::Client,
    config: &BuyerConfig,
    identity: &LocalIdentity,
    key: &VaultKey,
    label: &str,
) -> Result<Vec<u8>, BuyerError> {
    let owner = identity.agent_id().pubkey_base58();
    let path = vault_secret_path(&owner, label);
    let base = config.coordinator_url.trim_end_matches('/');
    let req = http.get(format!("{base}{path}"));
    let signing_path = vault_signing_path("GET", &path);
    let resp = send_signed(
        req,
        identity,
        &signing_path,
        &[],
        config,
        "fetch this secret",
    )
    .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(BuyerError::Coordinator(format!(
            "vault fetch returned {status}: {}",
            covenant_compute_protocol::coordinator_reason(&body)
        )));
    }
    let sealed: SealedSecret = resp
        .json()
        .await
        .map_err(|e| BuyerError::Coordinator(format!("vault fetch decode: {e}")))?;
    vault_open(key, &sealed, path.as_bytes())
        .map_err(|e| BuyerError::Protocol(format!("this key does not open that secret: {e}")))
}

/// Lists this buyer's secrets by label — metadata only, never a
/// ciphertext.
pub async fn vault_list(
    http: &reqwest::Client,
    config: &BuyerConfig,
    identity: &LocalIdentity,
) -> Result<Vec<SecretMeta>, BuyerError> {
    let owner = identity.agent_id().pubkey_base58();
    let path = vault_list_path(&owner);
    let base = config.coordinator_url.trim_end_matches('/');
    let req = http.get(format!("{base}{path}"));
    let signing_path = vault_signing_path("GET", &path);
    let resp = send_signed(
        req,
        identity,
        &signing_path,
        &[],
        config,
        "list your secrets",
    )
    .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(BuyerError::Coordinator(format!(
            "vault list returned {status}: {}",
            covenant_compute_protocol::coordinator_reason(&body)
        )));
    }
    resp.json()
        .await
        .map_err(|e| BuyerError::Coordinator(format!("vault list decode: {e}")))
}

/// Removes the secret at `label`. Idempotent: deleting one already gone
/// still succeeds.
pub async fn vault_delete(
    http: &reqwest::Client,
    config: &BuyerConfig,
    identity: &LocalIdentity,
    label: &str,
) -> Result<(), BuyerError> {
    let owner = identity.agent_id().pubkey_base58();
    let path = vault_secret_path(&owner, label);
    let base = config.coordinator_url.trim_end_matches('/');
    let req = http.delete(format!("{base}{path}"));
    let signing_path = vault_signing_path("DELETE", &path);
    let resp = send_signed(
        req,
        identity,
        &signing_path,
        &[],
        config,
        "delete this secret",
    )
    .await?;
    expect_success(resp, "vault delete").await
}

#[derive(Debug, thiserror::Error)]
pub enum KeyringError {
    #[error("vault keyring i/o at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("vault keyring at {path} is corrupt: {reason}")]
    Corrupt { path: String, reason: String },
    #[error("vault keyring holds an unreadable key for {label:?}: {reason}")]
    BadKey { label: String, reason: String },
    #[error("vault keyring at {path} is a symlink; refusing to follow it")]
    Symlink { path: String },
}

#[derive(Serialize, Deserialize)]
struct KeyringFile {
    version: u32,
    keys: BTreeMap<String, String>,
}

/// A buyer's local keyring for the client-sealed vault: the labels they
/// hold a sealing key for, mapped to that key. It lives on the buyer's own
/// disk and never crosses the wire. The coordinator stores ciphertext; this
/// stores the keys that open it, so the split the vault relies on holds by
/// where the bytes live, not by trust. Lose this file and every secret
/// sealed under it is unrecoverable, which is the vault's guarantee, not a
/// fault: back a key up with [`VaultKeyring::export`] before relying on one.
pub struct VaultKeyring {
    path: PathBuf,
    keys: BTreeMap<String, VaultKey>,
}

impl VaultKeyring {
    /// Loads the keyring at `path`, or an empty one when the file is
    /// absent. A file with looser permissions than `0600` is tightened in
    /// place on open, the way the buyer identity is; a symlink where the
    /// keyring should be is refused rather than followed.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, KeyringError> {
        let path = path.into();
        let path_str = path.display().to_string();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    path,
                    keys: BTreeMap::new(),
                });
            }
            Err(source) => {
                return Err(KeyringError::Io {
                    path: path_str,
                    source,
                })
            }
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.file_type().is_symlink() {
                return Err(KeyringError::Symlink { path: path_str });
            }
            let mode = meta.permissions().mode() & 0o777;
            if mode != 0o600 {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(
                    |source| KeyringError::Io {
                        path: path_str.clone(),
                        source,
                    },
                )?;
            }
        }
        let bytes = std::fs::read(&path).map_err(|source| KeyringError::Io {
            path: path_str.clone(),
            source,
        })?;
        let file: KeyringFile =
            serde_json::from_slice(&bytes).map_err(|e| KeyringError::Corrupt {
                path: path_str.clone(),
                reason: e.to_string(),
            })?;
        let mut keys = BTreeMap::new();
        for (label, b58) in file.keys {
            let key = VaultKey::from_b58(&b58).map_err(|e| KeyringError::BadKey {
                label: label.clone(),
                reason: e.to_string(),
            })?;
            keys.insert(label, key);
        }
        Ok(Self { path, keys })
    }

    /// The key for `label`, if this keyring holds one.
    pub fn get(&self, label: &str) -> Option<VaultKey> {
        self.keys.get(label).cloned()
    }

    pub fn contains(&self, label: &str) -> bool {
        self.keys.contains_key(label)
    }

    /// The key for `label`, minting and persisting a fresh one when none is
    /// held yet. The returned flag is `true` only when a key was just
    /// minted, so a caller can tell the buyer to back up a key that now
    /// exists nowhere else.
    pub fn ensure(&mut self, label: &str) -> Result<(VaultKey, bool), KeyringError> {
        if let Some(key) = self.keys.get(label) {
            return Ok((key.clone(), false));
        }
        let key = VaultKey::random();
        self.keys.insert(label.to_string(), key.clone());
        self.save()?;
        Ok((key, true))
    }

    /// Stores `key` for `label`, returning the key it replaced when one was
    /// already held. Used to import a key backed up or moved from another
    /// machine; the caller decides whether replacing a differing key is
    /// allowed.
    pub fn insert(&mut self, label: &str, key: VaultKey) -> Result<Option<VaultKey>, KeyringError> {
        let previous = self.keys.insert(label.to_string(), key);
        self.save()?;
        Ok(previous)
    }

    /// Drops the key for `label`, reporting whether one was held.
    /// Idempotent: dropping a label already gone does not touch the file.
    pub fn remove(&mut self, label: &str) -> Result<bool, KeyringError> {
        if self.keys.remove(label).is_none() {
            return Ok(false);
        }
        self.save()?;
        Ok(true)
    }

    /// Every label a key is held for, sorted.
    pub fn labels(&self) -> impl Iterator<Item = &str> {
        self.keys.keys().map(String::as_str)
    }

    fn save(&self) -> Result<(), KeyringError> {
        let file = KeyringFile {
            version: 1,
            keys: self
                .keys
                .iter()
                .map(|(label, key)| (label.clone(), key.to_b58()))
                .collect(),
        };
        let mut json = serde_json::to_vec_pretty(&file).map_err(|e| KeyringError::Corrupt {
            path: self.path.display().to_string(),
            reason: e.to_string(),
        })?;
        json.push(b'\n');
        write_secret_file(&self.path, &json)
    }
}

/// Writes `bytes` to `path` with owner-only permissions, through a
/// same-directory temp file and an atomic rename, so a crash mid-write
/// never leaves a half-written keyring and a reader never observes one.
#[cfg(unix)]
fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<(), KeyringError> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let io = |p: &Path, source: std::io::Error| KeyringError::Io {
        path: p.display().to_string(),
        source,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| io(&tmp, e))?;
    f.write_all(bytes).map_err(|e| io(&tmp, e))?;
    f.sync_all().map_err(|e| io(&tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| io(path, e))
}

#[cfg(not(unix))]
fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<(), KeyringError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| KeyringError::Io {
            path: parent.display().to_string(),
            source,
        })?;
    }
    std::fs::write(path, bytes).map_err(|source| KeyringError::Io {
        path: path.display().to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyring_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("home").join("vault-keys.json")
    }

    #[test]
    fn a_minted_key_persists_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = keyring_path(&dir);
        let minted = {
            let mut ring = VaultKeyring::open(&path).unwrap();
            let (key, minted) = ring.ensure("deploy").unwrap();
            assert!(minted, "a fresh label mints");
            key
        };
        let reopened = VaultKeyring::open(&path).unwrap();
        assert_eq!(
            reopened.get("deploy").unwrap().as_bytes(),
            minted.as_bytes(),
            "the same key survives a reopen"
        );
    }

    #[test]
    fn ensure_is_stable_for_a_known_label() {
        let dir = tempfile::tempdir().unwrap();
        let mut ring = VaultKeyring::open(keyring_path(&dir)).unwrap();
        let (first, minted_a) = ring.ensure("k").unwrap();
        let (second, minted_b) = ring.ensure("k").unwrap();
        assert!(minted_a && !minted_b, "only the first call mints");
        assert_eq!(first.as_bytes(), second.as_bytes());
    }

    #[test]
    fn insert_reports_the_key_it_replaced_and_remove_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let mut ring = VaultKeyring::open(keyring_path(&dir)).unwrap();
        let first = VaultKey::random();
        let second = VaultKey::random();
        assert!(ring.insert("k", first.clone()).unwrap().is_none());
        let replaced = ring.insert("k", second.clone()).unwrap().unwrap();
        assert_eq!(replaced.as_bytes(), first.as_bytes());
        assert_eq!(ring.get("k").unwrap().as_bytes(), second.as_bytes());
        assert!(ring.remove("k").unwrap());
        assert!(!ring.remove("k").unwrap(), "already gone");
        assert!(ring.get("k").is_none());
    }

    #[test]
    fn labels_are_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let mut ring = VaultKeyring::open(keyring_path(&dir)).unwrap();
        ring.ensure("b").unwrap();
        ring.ensure("a").unwrap();
        ring.ensure("c").unwrap();
        assert_eq!(ring.labels().collect::<Vec<_>>(), ["a", "b", "c"]);
    }

    #[test]
    fn an_absent_keyring_opens_empty() {
        let dir = tempfile::tempdir().unwrap();
        let ring = VaultKeyring::open(keyring_path(&dir)).unwrap();
        assert_eq!(ring.labels().count(), 0);
    }

    #[test]
    fn a_corrupt_keyring_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = keyring_path(&dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not json").unwrap();
        assert!(matches!(
            VaultKeyring::open(&path),
            Err(KeyringError::Corrupt { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn the_keyring_file_is_owner_only_and_a_loose_one_is_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = keyring_path(&dir);
        {
            let mut ring = VaultKeyring::open(&path).unwrap();
            ring.ensure("k").unwrap();
        }
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a written keyring is owner-only");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let _ = VaultKeyring::open(&path).unwrap();
        let tightened = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(tightened, 0o600, "a loose keyring is tightened on open");
    }
}
