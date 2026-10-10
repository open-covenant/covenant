//! Repository bundles too large to travel inside a job, stored under the
//! digest of their bytes. A buyer puts one before posting the task that
//! names it, and the operators fetch it by that digest and check it. A
//! task lives an hour at most, so bundles are kept for two days and then
//! removed, and the store as a whole stays under its cap.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use covenant_compute_protocol::{sha256_hex, MAX_STORED_BUNDLE_BYTES};

/// How long a stored bundle is kept.
pub const BUNDLE_MAX_AGE: Duration = Duration::from_secs(48 * 60 * 60);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PutError {
    #[error("the bytes do not hash to the digest they were put under")]
    Mismatch,
    #[error("a bundle of {0} bytes is over the {MAX_STORED_BUNDLE_BYTES}-byte cap")]
    TooLarge(u64),
    #[error("the bundle store is full; try again later")]
    Full,
    #[error("the bundle could not be written: {0}")]
    Io(String),
}

pub struct BundleStore {
    dir: PathBuf,
    max_bytes: u64,
}

impl BundleStore {
    pub fn open(dir: PathBuf, max_bytes: u64) -> std::io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir, max_bytes })
    }

    /// The stored file for a digest, if there is one.
    pub fn path(&self, sha256: &str) -> Option<PathBuf> {
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let path = self
            .dir
            .join(format!("{}.bundle", sha256.to_ascii_lowercase()));
        path.is_file().then_some(path)
    }

    /// Stores `bytes` under their digest. Putting a bundle already stored
    /// succeeds without writing it again.
    pub fn put(&self, sha256: &str, bytes: &[u8]) -> Result<(), PutError> {
        let size = bytes.len() as u64;
        if size > MAX_STORED_BUNDLE_BYTES {
            return Err(PutError::TooLarge(size));
        }
        if sha256_hex(bytes) != sha256 {
            return Err(PutError::Mismatch);
        }
        if self.path(sha256).is_some() {
            return Ok(());
        }
        self.sweep(BUNDLE_MAX_AGE);
        if self.used().saturating_add(size) > self.max_bytes {
            return Err(PutError::Full);
        }
        let io = |e: std::io::Error| PutError::Io(e.to_string());
        let partial = self.dir.join(format!("{sha256}.partial"));
        std::fs::write(&partial, bytes).map_err(io)?;
        std::fs::rename(&partial, self.dir.join(format!("{sha256}.bundle"))).map_err(io)
    }

    /// Removes bundles older than `max_age`, returning how many went.
    pub fn sweep(&self, max_age: Duration) -> usize {
        let now = SystemTime::now();
        self.files()
            .filter(|(_, meta)| {
                meta.modified()
                    .ok()
                    .and_then(|m| now.duration_since(m).ok())
                    .is_some_and(|age| age > max_age)
            })
            .filter(|(path, _)| std::fs::remove_file(path).is_ok())
            .count()
    }

    fn used(&self) -> u64 {
        self.files().map(|(_, meta)| meta.len()).sum()
    }

    fn files(&self) -> impl Iterator<Item = (PathBuf, std::fs::Metadata)> {
        std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| is_bundle(&entry.path()))
            .filter_map(|entry| Some((entry.path(), entry.metadata().ok()?)))
    }
}

fn is_bundle(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "bundle")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundle_is_kept_under_its_digest_and_the_cap_holds() {
        let dir = tempfile::tempdir().unwrap();
        let store = BundleStore::open(dir.path().join("bundles"), 10).unwrap();
        let bytes = b"bundle";
        let sha = sha256_hex(bytes);
        assert_eq!(store.put(&"0".repeat(64), bytes), Err(PutError::Mismatch));
        store.put(&sha, bytes).unwrap();
        store.put(&sha, bytes).unwrap();
        assert!(store.path(&sha).is_some());
        assert!(store.path("../etc/passwd").is_none());
        let more = b"more bytes";
        assert_eq!(store.put(&sha256_hex(more), more), Err(PutError::Full));
        assert_eq!(store.sweep(Duration::ZERO), 1);
        assert!(store.path(&sha).is_none());
    }
}
