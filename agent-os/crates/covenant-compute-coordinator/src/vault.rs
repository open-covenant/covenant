//! The coordinator's side of the client-sealed vault: a durable store of
//! ciphertext it cannot read. Every entry is a [`SealedSecret`] the buyer
//! produced under a key the coordinator never receives, so this file — and
//! anyone who reads it — holds opaque bytes and nothing more. The store's
//! only jobs are to keep a secret across a restart, to hand it back to the
//! owner who wrote it, and to refuse to grow without bound.
//!
//! Durability follows the buyer's purchase book exactly: an append-only
//! JSONL log with last-write-wins per key, a torn final line truncated on
//! open, boot compaction to one line per live entry via a temp file,
//! fsync, and atomic rename, and an `fsync` after every append. A delete
//! is a tombstone line, not an in-place edit, so the log stays append-only
//! and a crash can never leave a half-written record.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use covenant_compute_protocol::SealedSecret;
pub use covenant_compute_protocol::SecretMeta;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// How many live secrets one owner may keep. The store holds real bytes
/// on disk and in memory, and every write is signed by the owner, so the
/// bound is per-owner rather than global: it caps what one key can cost
/// the coordinator without letting one owner's fair use starve another's.
pub const MAX_SECRETS_PER_OWNER: usize = 64;

/// The largest base58 ciphertext the store will accept, matching the
/// plaintext limit the protocol seals under (64 KiB) with room for
/// base58's ~1.37x expansion and the authentication tag. A client that
/// seals through [`covenant_compute_protocol::vault_seal`] never
/// approaches it; the bound is here because the store cannot trust a
/// hand-rolled request to have respected the client-side limit.
pub const MAX_SEALED_B58_LEN: usize = 96 * 1024;

/// The largest base58 nonce the store will accept. The seal's nonce is a
/// fixed 24-byte XChaCha20 value, which base58-encodes to at most 33
/// characters; the bound sits above that with headroom and, like
/// [`MAX_SEALED_B58_LEN`], is here because the store cannot trust a
/// hand-rolled request to have sent a real nonce rather than a padded blob.
pub const MAX_NONCE_B58_LEN: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum VaultStoreError {
    #[error("owner already holds the maximum of {MAX_SECRETS_PER_OWNER} secrets")]
    OwnerFull,
    #[error("the vault is at its configured limit of {0} owners")]
    TooManyOwners(usize),
    #[error("sealed ciphertext is {0} bytes, over the {MAX_SEALED_B58_LEN}-byte limit")]
    TooLarge(usize),
    #[error("sealed nonce is {0} bytes, over the {MAX_NONCE_B58_LEN}-byte limit")]
    NonceTooLarge(usize),
    #[error("vault store persistence: {0}")]
    Persist(String),
}

/// One entry as it rests on disk. The owner's base58 pubkey and the label
/// are the key; a tombstone (`deleted = true`) supersedes an earlier live
/// record for the same key on replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredRecord {
    owner: String,
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    sealed: Option<SealedSecret>,
    created_at_ms: u64,
    updated_at_ms: u64,
    #[serde(default)]
    deleted: bool,
}

struct Live {
    sealed: SealedSecret,
    created_at_ms: u64,
    updated_at_ms: u64,
}

pub struct VaultStore {
    entries: Mutex<HashMap<(String, String), Live>>,
    file: Option<Mutex<std::fs::File>>,
    /// A ceiling on distinct owners, or `None` for no ceiling. Keypairs
    /// are free to mint, so an enabled public vault wants a volumetric
    /// backstop the way the operator registry does — set by the
    /// deployment, unlimited by default.
    max_owners: Option<usize>,
}

impl VaultStore {
    pub fn in_memory() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            file: None,
            max_owners: None,
        }
    }

    /// Bounds the number of distinct owners the store will admit. An
    /// existing owner is never turned away by it; only a brand-new owner
    /// arriving at the ceiling is.
    pub fn with_max_owners(mut self, max_owners: Option<usize>) -> Self {
        self.max_owners = max_owners;
        self
    }

    pub fn open(path: &Path) -> Result<Self, VaultStoreError> {
        let persist = |e: std::io::Error| VaultStoreError::Persist(e.to_string());

        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(persist(e)),
        };
        let valid_len = bytes
            .iter()
            .rposition(|&b| b == b'\n')
            .map(|p| p + 1)
            .unwrap_or(0);
        if valid_len < bytes.len() {
            tracing::warn!(
                dropped_bytes = bytes.len() - valid_len,
                "truncating torn final vault-store record"
            );
            let f = std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .map_err(persist)?;
            f.set_len(valid_len as u64).map_err(persist)?;
        }

        let mut records: HashMap<(String, String), StoredRecord> = HashMap::new();
        let mut lines = 0usize;
        for (i, line) in bytes[..valid_len].split(|&b| b == b'\n').enumerate() {
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            lines += 1;
            let record: StoredRecord = serde_json::from_slice(line)
                .map_err(|e| VaultStoreError::Persist(format!("line {} is corrupt: {e}", i + 1)))?;
            records.insert((record.owner.clone(), record.label.clone()), record);
        }

        let live: HashMap<(String, String), Live> = records
            .into_iter()
            .filter(|(_, r)| !r.deleted)
            .filter_map(|(key, r)| {
                r.sealed.map(|sealed| {
                    (
                        key,
                        Live {
                            sealed,
                            created_at_ms: r.created_at_ms,
                            updated_at_ms: r.updated_at_ms,
                        },
                    )
                })
            })
            .collect();

        // Boot compaction: replay just proved which entries are live, so
        // rewrite the log down to one line each — via temp file, fsync,
        // atomic rename, so a crash mid-compaction leaves the original
        // intact. Only when history has piled up past the live set.
        if lines > live.len() {
            use std::io::Write;
            let mut ordered: Vec<(&(String, String), &Live)> = live.iter().collect();
            ordered.sort_by(|a, b| a.0.cmp(b.0));
            let mut buf = Vec::with_capacity(valid_len / 2);
            for ((owner, label), entry) in ordered {
                let record = StoredRecord {
                    owner: owner.clone(),
                    label: label.clone(),
                    sealed: Some(entry.sealed.clone()),
                    created_at_ms: entry.created_at_ms,
                    updated_at_ms: entry.updated_at_ms,
                    deleted: false,
                };
                let line = serde_json::to_vec(&record)
                    .map_err(|e| VaultStoreError::Persist(e.to_string()))?;
                buf.extend_from_slice(&line);
                buf.push(b'\n');
            }
            let tmp_path = path.with_extension("jsonl.compacting");
            let mut tmp = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp_path)
                .map_err(persist)?;
            tmp.write_all(&buf).map_err(persist)?;
            tmp.sync_all().map_err(persist)?;
            std::fs::rename(&tmp_path, path).map_err(persist)?;
            tracing::info!(
                lines_before = lines,
                entries_after = live.len(),
                "compacted vault store"
            );
        }

        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(persist)?;
        Ok(Self {
            entries: Mutex::new(live),
            file: Some(Mutex::new(file)),
            max_owners: None,
        })
    }

    fn append(&self, record: &StoredRecord) -> Result<(), VaultStoreError> {
        use std::io::Write;
        let Some(file) = &self.file else {
            return Ok(());
        };
        let persist = |e: std::io::Error| VaultStoreError::Persist(e.to_string());
        let mut line =
            serde_json::to_vec(record).map_err(|e| VaultStoreError::Persist(e.to_string()))?;
        line.push(b'\n');
        let mut file = file.lock();
        file.write_all(&line).map_err(persist)?;
        // fsync, not flush: a stored secret must survive a power loss, not
        // just a clean kill — a buyer who wrote one and moved on cannot be
        // told after a crash that it never landed.
        file.sync_data().map_err(persist)
    }

    /// Stores (or replaces) a secret. Writing a new label when the owner
    /// is already at [`MAX_SECRETS_PER_OWNER`] is refused; replacing a
    /// label the owner already holds always succeeds. The durable append
    /// happens before the in-memory map is updated, so a crash cannot
    /// leave a secret readable that was never persisted.
    pub fn put(
        &self,
        owner: &str,
        label: &str,
        sealed: SealedSecret,
        now_ms: u64,
    ) -> Result<(), VaultStoreError> {
        if sealed.ciphertext.len() > MAX_SEALED_B58_LEN {
            return Err(VaultStoreError::TooLarge(sealed.ciphertext.len()));
        }
        if sealed.nonce.len() > MAX_NONCE_B58_LEN {
            return Err(VaultStoreError::NonceTooLarge(sealed.nonce.len()));
        }
        let key = (owner.to_string(), label.to_string());
        let mut entries = self.entries.lock();
        let existing = entries.get(&key);
        if existing.is_none() {
            let owned = entries.keys().filter(|(o, _)| o == owner).count();
            if owned >= MAX_SECRETS_PER_OWNER {
                return Err(VaultStoreError::OwnerFull);
            }
            // A brand-new owner (none of their labels are present) counts
            // against the global ceiling before it is admitted.
            if owned == 0 {
                if let Some(cap) = self.max_owners {
                    let distinct = entries
                        .keys()
                        .map(|(o, _)| o.as_str())
                        .collect::<HashSet<_>>()
                        .len();
                    if distinct >= cap {
                        return Err(VaultStoreError::TooManyOwners(cap));
                    }
                }
            }
        }
        let created_at_ms = existing.map(|e| e.created_at_ms).unwrap_or(now_ms);
        let record = StoredRecord {
            owner: owner.to_string(),
            label: label.to_string(),
            sealed: Some(sealed.clone()),
            created_at_ms,
            updated_at_ms: now_ms,
            deleted: false,
        };
        self.append(&record)?;
        entries.insert(
            key,
            Live {
                sealed,
                created_at_ms,
                updated_at_ms: now_ms,
            },
        );
        Ok(())
    }

    pub fn get(&self, owner: &str, label: &str) -> Option<SealedSecret> {
        self.entries
            .lock()
            .get(&(owner.to_string(), label.to_string()))
            .map(|e| e.sealed.clone())
    }

    /// Every live secret this owner holds, by label, sorted for a stable
    /// listing. Metadata only — the ciphertext is never in a listing.
    pub fn list(&self, owner: &str) -> Vec<SecretMeta> {
        let entries = self.entries.lock();
        let mut out: Vec<SecretMeta> = entries
            .iter()
            .filter(|((o, _), _)| o == owner)
            .map(|((_, label), e)| SecretMeta {
                label: label.clone(),
                ciphertext_len: e.sealed.ciphertext.len(),
                created_at_ms: e.created_at_ms,
                updated_at_ms: e.updated_at_ms,
            })
            .collect();
        out.sort_by(|a, b| a.label.cmp(&b.label));
        out
    }

    /// Removes a secret, returning whether one was there. A tombstone is
    /// appended before the map drops the entry, so the removal survives a
    /// restart just as a write does.
    pub fn delete(&self, owner: &str, label: &str, now_ms: u64) -> Result<bool, VaultStoreError> {
        let key = (owner.to_string(), label.to_string());
        let mut entries = self.entries.lock();
        if !entries.contains_key(&key) {
            return Ok(false);
        }
        let record = StoredRecord {
            owner: owner.to_string(),
            label: label.to_string(),
            sealed: None,
            created_at_ms: 0,
            updated_at_ms: now_ms,
            deleted: true,
        };
        self.append(&record)?;
        entries.remove(&key);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_compute_protocol::{vault_seal, VaultKey};

    fn sealed(text: &[u8]) -> SealedSecret {
        // The store keeps opaque bytes and never opens them, so the bound
        // context here is immaterial to what these tests exercise.
        vault_seal(&VaultKey::random(), text, b"test-context").unwrap()
    }

    #[test]
    fn a_stored_secret_reads_back_and_a_missing_one_does_not() {
        let store = VaultStore::in_memory();
        let s = sealed(b"token");
        store.put("owner-a", "deploy", s.clone(), 10).unwrap();
        assert_eq!(store.get("owner-a", "deploy"), Some(s));
        assert_eq!(store.get("owner-a", "absent"), None);
        assert_eq!(store.get("owner-b", "deploy"), None);
    }

    #[test]
    fn a_replacement_keeps_the_original_creation_time() {
        let store = VaultStore::in_memory();
        store.put("o", "k", sealed(b"v1"), 100).unwrap();
        let second = sealed(b"v2");
        store.put("o", "k", second.clone(), 200).unwrap();
        assert_eq!(store.get("o", "k"), Some(second));
        let meta = &store.list("o")[0];
        assert_eq!(meta.created_at_ms, 100);
        assert_eq!(meta.updated_at_ms, 200);
    }

    #[test]
    fn a_listing_is_scoped_to_the_owner_and_holds_no_ciphertext() {
        let store = VaultStore::in_memory();
        store.put("o", "b", sealed(b"x"), 1).unwrap();
        store.put("o", "a", sealed(b"x"), 1).unwrap();
        store.put("other", "c", sealed(b"x"), 1).unwrap();
        let labels: Vec<String> = store.list("o").into_iter().map(|m| m.label).collect();
        assert_eq!(labels, ["a", "b"], "sorted and scoped to the owner");
    }

    #[test]
    fn a_delete_removes_the_secret_and_reports_whether_one_was_there() {
        let store = VaultStore::in_memory();
        store.put("o", "k", sealed(b"v"), 1).unwrap();
        assert!(store.delete("o", "k", 2).unwrap());
        assert_eq!(store.get("o", "k"), None);
        assert!(!store.delete("o", "k", 3).unwrap(), "already gone");
    }

    #[test]
    fn an_owner_is_capped_but_a_replacement_is_not() {
        let store = VaultStore::in_memory();
        for i in 0..MAX_SECRETS_PER_OWNER {
            store.put("o", &format!("k{i}"), sealed(b"v"), 1).unwrap();
        }
        assert!(matches!(
            store.put("o", "one-more", sealed(b"v"), 1),
            Err(VaultStoreError::OwnerFull)
        ));
        // Replacing a label already held stays allowed at the cap.
        store.put("o", "k0", sealed(b"v2"), 2).unwrap();
        // A different owner is unaffected by the first owner's fill.
        store.put("other", "k", sealed(b"v"), 1).unwrap();
    }

    #[test]
    fn a_global_owner_ceiling_admits_known_owners_but_turns_new_ones_away() {
        let store = VaultStore::in_memory().with_max_owners(Some(2));
        store.put("a", "k", sealed(b"v"), 1).unwrap();
        store.put("b", "k", sealed(b"v"), 1).unwrap();
        // A third distinct owner is refused at the ceiling.
        assert!(matches!(
            store.put("c", "k", sealed(b"v"), 1),
            Err(VaultStoreError::TooManyOwners(2))
        ));
        // The two already admitted keep writing — more labels, replacements.
        store.put("a", "k2", sealed(b"v"), 1).unwrap();
        store.put("b", "k", sealed(b"v2"), 2).unwrap();
        // Emptying an owner frees its slot for a new one.
        store.delete("a", "k", 3).unwrap();
        store.delete("a", "k2", 3).unwrap();
        store.put("c", "k", sealed(b"v"), 4).unwrap();
    }

    #[test]
    fn oversized_ciphertext_is_refused() {
        let store = VaultStore::in_memory();
        let huge = SealedSecret {
            nonce: "n".into(),
            ciphertext: "c".repeat(MAX_SEALED_B58_LEN + 1),
        };
        assert!(matches!(
            store.put("o", "k", huge, 1),
            Err(VaultStoreError::TooLarge(_))
        ));
    }

    #[test]
    fn an_oversized_nonce_is_refused() {
        // A real 24-byte nonce base58-encodes well under the bound; a
        // hand-rolled request padding the nonce to bloat the durable store
        // is refused the same way an oversized ciphertext is.
        let store = VaultStore::in_memory();
        let bloated = SealedSecret {
            nonce: "1".repeat(MAX_NONCE_B58_LEN + 1),
            ..sealed(b"v")
        };
        assert!(matches!(
            store.put("o", "k", bloated, 1),
            Err(VaultStoreError::NonceTooLarge(_))
        ));
        store.put("o", "k", sealed(b"v"), 1).unwrap();
    }

    #[test]
    fn secrets_survive_a_reopen_and_deletes_stay_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.jsonl");
        let keep = sealed(b"keep");
        {
            let store = VaultStore::open(&path).unwrap();
            store.put("o", "keep", keep.clone(), 1).unwrap();
            store.put("o", "drop", sealed(b"drop"), 1).unwrap();
            store.delete("o", "drop", 2).unwrap();
        }
        let reopened = VaultStore::open(&path).unwrap();
        assert_eq!(reopened.get("o", "keep"), Some(keep));
        assert_eq!(reopened.get("o", "drop"), None);
        assert_eq!(reopened.list("o").len(), 1);
    }

    #[test]
    fn a_reopen_compacts_history_to_the_live_set() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.jsonl");
        {
            let store = VaultStore::open(&path).unwrap();
            for v in 0..5 {
                store
                    .put("o", "k", sealed(format!("v{v}").as_bytes()), v)
                    .unwrap();
            }
        }
        let lines_before = std::fs::read_to_string(&path).unwrap().lines().count();
        assert_eq!(lines_before, 5, "one appended line per write");
        // Reopen compacts the five writes of one key down to a single line.
        let _ = VaultStore::open(&path).unwrap();
        let lines_after = std::fs::read_to_string(&path).unwrap().lines().count();
        assert_eq!(lines_after, 1);
    }

    #[test]
    fn a_torn_final_line_is_dropped_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.jsonl");
        let good = sealed(b"good");
        {
            let store = VaultStore::open(&path).unwrap();
            store.put("o", "good", good.clone(), 1).unwrap();
        }
        // Append a half-written record with no trailing newline, as a
        // crash mid-append would leave.
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(br#"{"owner":"o","label":"torn""#).unwrap();
        }
        let store = VaultStore::open(&path).unwrap();
        assert_eq!(store.get("o", "good"), Some(good));
        assert_eq!(store.get("o", "torn"), None);
    }

    #[test]
    fn a_corrupt_record_fails_the_open_rather_than_dropping_a_secret() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.jsonl");
        {
            let store = VaultStore::open(&path).unwrap();
            store.put("o", "good", sealed(b"good"), 1).unwrap();
        }
        // A newline-terminated garbage line is damage, not a torn tail.
        // Skipping it would silently drop a stored secret across a
        // restart, so the open fails hard instead.
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(b"{not a vault record}\n").unwrap();
        }
        let err = match VaultStore::open(&path) {
            Err(e) => e,
            Ok(_) => panic!("a corrupt record must fail the open, not open silently"),
        };
        assert!(
            matches!(err, VaultStoreError::Persist(ref m) if m.contains("line 2") && m.contains("corrupt")),
            "got: {err:?}"
        );
    }
}
