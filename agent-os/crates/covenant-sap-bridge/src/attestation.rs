//! Publish audit-root attestations into SAP.
//!
//! Skeleton. Only the root hash and a small structured envelope go
//! on-chain — never the underlying audit log contents.

use serde::{Deserialize, Serialize};

use crate::client::SapBridge;
use crate::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRootAttestation {
    pub root_hash_hex: String,
    pub release_target: String,
    pub release_subject: String,
    pub release_scope: String,
    pub recorded_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishedAttestation {
    pub attestation_pda: String,
    pub signature: String,
}

impl SapBridge {
    pub async fn publish_audit_root(
        &self,
        _attestation: &AuditRootAttestation,
    ) -> Result<PublishedAttestation> {
        self.require_enabled()?;
        todo!("submit attestation tx via the bridge worker")
    }
}
