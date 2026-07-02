//! Append-only, hash-chained event log. Each entry commits to the one before
//! it, so a receipt built from the chain can be checked after the fact: change
//! any past event and every hash from that point breaks.
//!
//! The chain is deliberately small and self-contained: one file, one hash
//! function, a `verify` that a reader can follow without trusting us.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Mutex;

/// The all-zero hash the first entry chains onto.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// One committed event. `hash` covers every other field including `prev`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub seq: u64,
    pub ts_ms: u64,
    pub kind: String,
    pub data: serde_json::Value,
    pub prev: String,
    pub hash: String,
}

/// The bytes that get hashed: the entry without its own hash. Kept as a
/// separate struct so signing and verification hash exactly the same shape.
#[derive(Serialize)]
struct Preimage<'a> {
    seq: u64,
    ts_ms: u64,
    kind: &'a str,
    data: &'a serde_json::Value,
    prev: &'a str,
}

fn hash_entry(seq: u64, ts_ms: u64, kind: &str, data: &serde_json::Value, prev: &str) -> String {
    let pre = Preimage {
        seq,
        ts_ms,
        kind,
        data,
        prev,
    };
    let canonical = serde_jcs::to_vec(&pre).expect("jcs canonicalization of a plain struct");
    let digest = Sha256::digest(&canonical);
    hex(&digest)
}

/// Lowercase hex, no dependency on an encoding crate.
pub fn hex(bytes: &[u8]) -> String {
    const HEXCHARS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEXCHARS[(b >> 4) as usize] as char);
        s.push(HEXCHARS[(b & 0x0f) as usize] as char);
    }
    s
}

/// A growing chain. `append` is cheap and locks only briefly, so it is safe to
/// call from the proxy's request path.
#[derive(Default)]
pub struct Chain {
    inner: Mutex<Vec<Entry>>,
}

impl Chain {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an event and return its committed hash (the new chain head).
    pub fn append(&self, ts_ms: u64, kind: &str, data: serde_json::Value) -> String {
        let mut v = self.inner.lock().unwrap();
        let seq = v.len() as u64;
        let prev = v
            .last()
            .map(|e| e.hash.clone())
            .unwrap_or_else(|| GENESIS.to_string());
        let hash = hash_entry(seq, ts_ms, kind, &data, &prev);
        v.push(Entry {
            seq,
            ts_ms,
            kind: kind.to_string(),
            data,
            prev,
            hash: hash.clone(),
        });
        hash
    }

    pub fn head(&self) -> String {
        self.inner
            .lock()
            .unwrap()
            .last()
            .map(|e| e.hash.clone())
            .unwrap_or_else(|| GENESIS.to_string())
    }

    pub fn len(&self) -> u64 {
        self.inner.lock().unwrap().len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap().is_empty()
    }

    pub fn snapshot(&self) -> Vec<Entry> {
        self.inner.lock().unwrap().clone()
    }
}

/// Why a chain failed to verify.
#[derive(Debug)]
pub enum ChainError {
    Sequence { at: usize },
    Link { at: usize },
    Hash { at: usize },
}

impl std::fmt::Display for ChainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChainError::Sequence { at } => write!(f, "entry {at} has the wrong sequence number"),
            ChainError::Link { at } => write!(f, "entry {at} does not link to the previous hash"),
            ChainError::Hash { at } => write!(f, "entry {at} hash does not match its contents"),
        }
    }
}

/// Recompute every hash and confirm the chain is intact. Returns the head hash
/// on success. An empty chain verifies to `GENESIS`.
pub fn verify(entries: &[Entry]) -> Result<String, ChainError> {
    let mut prev = GENESIS.to_string();
    for (i, e) in entries.iter().enumerate() {
        if e.seq != i as u64 {
            return Err(ChainError::Sequence { at: i });
        }
        if e.prev != prev {
            return Err(ChainError::Link { at: i });
        }
        let recomputed = hash_entry(e.seq, e.ts_ms, &e.kind, &e.data, &e.prev);
        if recomputed != e.hash {
            return Err(ChainError::Hash { at: i });
        }
        prev = e.hash.clone();
    }
    Ok(prev)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn appends_link_and_verify() {
        let c = Chain::new();
        c.append(1, "run_start", json!({"agent": "claude"}));
        c.append(2, "api_call", json!({"cost_usd": 0.12}));
        let head = c.append(3, "run_end", json!({"outcome": "completed"}));
        let entries = c.snapshot();
        assert_eq!(entries.len(), 3);
        assert_eq!(verify(&entries).unwrap(), head);
    }

    #[test]
    fn tamper_breaks_verification() {
        let c = Chain::new();
        c.append(1, "run_start", json!({"agent": "claude"}));
        c.append(2, "api_call", json!({"cost_usd": 0.12}));
        let mut entries = c.snapshot();
        // Rewrite a committed cost, the kind of edit a receipt has to catch.
        entries[1].data = json!({"cost_usd": 0.00});
        assert!(matches!(verify(&entries), Err(ChainError::Hash { at: 1 })));
    }

    #[test]
    fn reordering_breaks_the_link() {
        let c = Chain::new();
        c.append(1, "a", json!({}));
        c.append(2, "b", json!({}));
        let mut entries = c.snapshot();
        entries.swap(0, 1);
        assert!(verify(&entries).is_err());
    }
}
