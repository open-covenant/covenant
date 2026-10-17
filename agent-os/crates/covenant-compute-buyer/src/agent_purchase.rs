//! Durable agent/check purchases. A key always names one signed order, even
//! when the order fails; a caller deliberately chooses a new key for new work.
use std::{
    fs::File,
    path::{Path, PathBuf},
};

use covenant_identity::LocalIdentity;
use uuid::Uuid;

use crate::{BuyerConfig, BuyerError, JobRequest, PurchaseBook, PurchaseEntry, SignedJobEnvelope};

pub(crate) struct AgentPurchase {
    _lock: File,
    book: PurchaseBook,
    key: String,
    directory: PathBuf,
}

fn invalid(message: impl ToString) -> BuyerError {
    BuyerError::Protocol(message.to_string())
}

impl AgentPurchase {
    pub(crate) fn open(
        config: &BuyerConfig,
        identity: &LocalIdentity,
        key: &str,
    ) -> Result<Self, BuyerError> {
        if !(1..=128).contains(&key.len()) {
            return Err(invalid("idempotency_key must be 1..=128 bytes"));
        }
        let home = match std::env::var("COVENANT_COMPUTE_MCP_HOME") {
            Ok(value) if !value.is_empty() => PathBuf::from(value),
            Err(std::env::VarError::NotPresent) => {
                PathBuf::from(std::env::var("HOME").map_err(|_| invalid("HOME is required"))?)
                    .join(".covenant-compute-mcp")
            }
            _ => return Err(invalid("invalid buyer home")),
        };
        Self::open_at(&home, config, identity, key)
    }

    fn open_at(
        home: &Path,
        config: &BuyerConfig,
        identity: &LocalIdentity,
        key: &str,
    ) -> Result<Self, BuyerError> {
        let endpoint =
            crate::durable_budget::canonical_endpoint(&config.coordinator_url).map_err(invalid)?;
        let key = serde_json::to_string(&(identity.agent_id().pubkey_base58(), endpoint, key))
            .map_err(invalid)?;
        let directory = home
            .join("agent-purchases")
            .join(crate::sha256_hex(key.as_bytes()));
        std::fs::create_dir_all(&directory).map_err(invalid)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                .map_err(invalid)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(directory.join("lock")).map_err(invalid)?;
        lock.try_lock().map_err(|e| {
            invalid(format!(
                "idempotent purchase is busy or unavailable; retry the same key: {e}"
            ))
        })?;
        let book = PurchaseBook::open(&directory.join("purchase.jsonl")).map_err(invalid)?;
        Ok(Self {
            _lock: lock,
            book,
            key,
            directory,
        })
    }

    pub(crate) fn recorded(&self) -> bool {
        self.book.lookup(&self.key).is_some()
    }

    pub(crate) fn job_id(&self) -> Uuid {
        self.book
            .lookup(&self.key)
            .map(|e| e.envelope.payload.job_id)
            .unwrap_or_else(Uuid::new_v4)
    }

    pub(crate) fn resolve(
        &self,
        config: &BuyerConfig,
        identity: &LocalIdentity,
        request: JobRequest,
        job_id: Uuid,
    ) -> Result<SignedJobEnvelope, BuyerError> {
        if let Some(entry) = self.book.lookup(&self.key) {
            if let Some(argument) = entry.conflicting_argument(
                request.kind,
                &request.input,
                request.model.as_deref(),
                request.gpu_class.as_deref(),
                request.min_vram_gb,
                request.min_reputation_bps,
                Some(request.price_micro_usdc),
                Some(request.deadline_ms),
            ) {
                return Err(invalid(format!("idempotency key conflicts with {argument}; use the original order or a new key")));
            }
            return Ok(entry.envelope);
        }
        let envelope = crate::build_envelope(config, identity, request, false, job_id)?;
        self.book
            .record(PurchaseEntry {
                key: self.key.clone(),
                envelope: envelope.clone(),
                opened_at_ms: crate::epoch_ms(),
                receipt_id: None,
                voided: false,
            })
            .map_err(invalid)?;
        File::open(&self.directory)
            .and_then(|f| f.sync_all())
            .map_err(invalid)?;
        File::open(
            self.directory
                .parent()
                .expect("purchase directory has parent"),
        )
        .and_then(|f| f.sync_all())
        .map_err(invalid)?;
        Ok(envelope)
    }

    pub(crate) fn settle(&self, id: Uuid) -> Result<(), BuyerError> {
        self.book.settle(&self.key, id).map_err(invalid)
    }
}

/// Whether this exact buyer/endpoint/key already names an agent or check order.
/// Used by session frontends to avoid charging their local cap twice on replay.
pub fn agent_purchase_recorded(
    config: &BuyerConfig,
    identity: &LocalIdentity,
    key: &str,
) -> Result<bool, BuyerError> {
    Ok(AgentPurchase::open(config, identity, key)?.recorded())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config(endpoint: &str) -> BuyerConfig {
        BuyerConfig {
            coordinator_url: endpoint.into(),
            poll_interval: std::time::Duration::from_millis(1),
            referral_code: None,
            rpc_url: None,
        }
    }
    fn request(price: u64) -> JobRequest {
        JobRequest {
            kind: crate::JobKind::InferenceCall,
            input: vec![],
            model: Some("test".into()),
            gpu_class: None,
            min_vram_gb: None,
            min_reputation_bps: None,
            price_micro_usdc: price,
            deadline_ms: 60000,
        }
    }
    #[test]
    fn restart_replays_exact_bytes_and_conflicts_never_rebuy() {
        let home = tempfile::tempdir().unwrap();
        let buyer = LocalIdentity::generate("agent@local");
        let config = config("https://compute.example/");
        let purchase = AgentPurchase::open_at(home.path(), &config, &buyer, "proof").unwrap();
        assert!(!purchase.recorded());
        let first = purchase
            .resolve(&config, &buyer, request(100), purchase.job_id())
            .unwrap();
        assert!(AgentPurchase::open_at(home.path(), &config, &buyer, "proof").is_err());
        purchase.settle(first.payload.job_id).unwrap();
        drop(purchase);
        let restarted = AgentPurchase::open_at(home.path(), &config, &buyer, "proof").unwrap();
        let replay = restarted
            .resolve(&config, &buyer, request(100), restarted.job_id())
            .unwrap();
        assert_eq!(
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec(&replay).unwrap()
        );
        assert!(restarted
            .resolve(&config, &buyer, request(101), restarted.job_id())
            .is_err());
        let other = AgentPurchase::open_at(
            home.path(),
            &self::config("https://other.example"),
            &buyer,
            "proof",
        )
        .unwrap();
        assert!(!other.recorded());
    }
}
