//! Signed provenance for served inference.
//!
//! On a successful upstream response the gateway commits to what ran — the model
//! identity, the exact prompt, and the exact output — as an
//! [`InferenceReceiptPayload`] whose hash is returned to the caller and appended
//! here. The commitment is to *hashes only*: the raw prompt and the raw output are
//! hashed and dropped in the same breath. Nothing in this module ever holds, stores
//! or logs the plaintext of either, which is the whole point — the receipt proves an
//! output came from a model over a prompt without the gateway keeping the words.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The ring keeps this many receipts for the `/v1/receipts` window; older ones age
/// out. The append-only log, when configured, keeps them all.
pub const DEFAULT_RING_CAPACITY: usize = 1_000;

/// One receipt as it is stored and served. Every field is a hash or accounting
/// metadata — never a prompt or a completion. Adding a plaintext field here would
/// break the module's only real invariant, so don't.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReceiptRecord {
    pub receipt_hash: String,
    pub model_identity_digest: String,
    pub prompt_hash: String,
    pub output_hash: String,
    pub node_id: String,
    pub model: String,
    pub timestamp: i64,
    pub amount_micro: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment_reference: Option<String>,
}

/// A bounded in-memory ring of recent receipts, optionally mirrored to an
/// append-only log. Cheap to clone-read; writes take a short lock.
pub struct ReceiptStore {
    ring: Mutex<VecDeque<ReceiptRecord>>,
    capacity: usize,
    log: Option<Mutex<File>>,
}

impl ReceiptStore {
    pub fn in_memory() -> Self {
        Self {
            ring: Mutex::new(VecDeque::new()),
            capacity: DEFAULT_RING_CAPACITY,
            log: None,
        }
    }

    /// Opens (creating if absent) an append-only receipt log. Each receipt is one
    /// JSON line; the file inherits the same hashes-only guarantee as the ring.
    pub fn with_log(path: &Path) -> anyhow::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("open receipts log {}", path.display()))?;
        Ok(Self {
            ring: Mutex::new(VecDeque::new()),
            capacity: DEFAULT_RING_CAPACITY,
            log: Some(Mutex::new(file)),
        })
    }

    pub fn append(&self, record: ReceiptRecord) {
        if let Some(log) = &self.log {
            match serde_json::to_string(&record) {
                Ok(line) => {
                    let mut file = log.lock().expect("receipts log mutex poisoned");
                    if let Err(error) = writeln!(file, "{line}") {
                        tracing::warn!(%error, "append receipt to log");
                    }
                }
                Err(error) => tracing::warn!(%error, "serialize receipt for log"),
            }
        }
        let mut ring = self.ring.lock().expect("receipts ring mutex poisoned");
        if ring.len() == self.capacity {
            ring.pop_front();
        }
        ring.push_back(record);
    }

    /// The most recent receipts, newest first, capped at `limit`.
    pub fn recent(&self, limit: usize) -> Vec<ReceiptRecord> {
        let ring = self.ring.lock().expect("receipts ring mutex poisoned");
        ring.iter().rev().take(limit).cloned().collect()
    }

    pub fn get(&self, hash: &str) -> Option<ReceiptRecord> {
        let ring = self.ring.lock().expect("receipts ring mutex poisoned");
        ring.iter().rev().find(|r| r.receipt_hash == hash).cloned()
    }

    pub fn len(&self) -> usize {
        self.ring
            .lock()
            .expect("receipts ring mutex poisoned")
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_inference_protocol::{receipt_hash, InferenceReceiptPayload};
    use std::io::Read;

    const PROMPT: &str = "the treasury seed phrase is correct horse battery staple";
    const OUTPUT: &str = "acknowledged, i will never repeat that phrase";

    fn record_for(prompt: &str, output: &str) -> ReceiptRecord {
        let payload = InferenceReceiptPayload {
            model_identity_digest: "d".repeat(64),
            prompt_hash: hash_bytes(prompt.as_bytes()),
            output_hash: hash_bytes(output.as_bytes()),
            node_id: "0xnode".into(),
            timestamp: now_unix(),
        };
        ReceiptRecord {
            receipt_hash: receipt_hash(&payload).unwrap(),
            model_identity_digest: payload.model_identity_digest,
            prompt_hash: payload.prompt_hash,
            output_hash: payload.output_hash,
            node_id: payload.node_id,
            model: "qwen3:8b".into(),
            timestamp: payload.timestamp,
            amount_micro: 1_000,
            payment_reference: Some("pay-1".into()),
        }
    }

    #[test]
    fn receipt_hash_matches_an_independently_built_payload() {
        let record = record_for(PROMPT, OUTPUT);
        let rebuilt = InferenceReceiptPayload {
            model_identity_digest: record.model_identity_digest.clone(),
            prompt_hash: record.prompt_hash.clone(),
            output_hash: record.output_hash.clone(),
            node_id: record.node_id.clone(),
            timestamp: record.timestamp,
        };
        assert_eq!(record.receipt_hash, receipt_hash(&rebuilt).unwrap());
    }

    #[test]
    fn store_and_log_never_hold_the_plaintext() {
        let dir = std::env::temp_dir().join(format!("covenant-receipts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("receipts.log");

        let store = ReceiptStore::with_log(&log_path).unwrap();
        store.append(record_for(PROMPT, OUTPUT));

        let served = serde_json::to_string(&store.recent(10)).unwrap();
        assert!(!served.contains(PROMPT), "ring leaked the prompt plaintext");
        assert!(!served.contains(OUTPUT), "ring leaked the output plaintext");

        let mut on_disk = String::new();
        File::open(&log_path)
            .unwrap()
            .read_to_string(&mut on_disk)
            .unwrap();
        assert!(!on_disk.contains(PROMPT), "log leaked the prompt plaintext");
        assert!(!on_disk.contains(OUTPUT), "log leaked the output plaintext");
        assert!(on_disk.contains(&hash_bytes(PROMPT.as_bytes())));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ring_evicts_oldest_past_capacity_and_reads_newest_first() {
        let store = ReceiptStore {
            ring: Mutex::new(VecDeque::new()),
            capacity: 2,
            log: None,
        };
        for i in 0..3 {
            let mut record = record_for(PROMPT, OUTPUT);
            record.receipt_hash = format!("hash-{i}");
            store.append(record);
        }
        let recent = store.recent(10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].receipt_hash, "hash-2");
        assert_eq!(recent[1].receipt_hash, "hash-1");
        assert!(store.get("hash-0").is_none());
        assert_eq!(store.get("hash-2").unwrap().amount_micro, 1_000);
    }
}
