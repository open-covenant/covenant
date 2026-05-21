//! Publish audit-root attestations into SAP.
//!
//! Only the 32-byte Merkle root and a small structured envelope go
//! on-chain — never the underlying audit-log contents. The SAP
//! attestation module is the public verification and interoperability
//! layer external parties read to confirm Covenant roots.

use serde::{Deserialize, Serialize};

use crate::client::SapBridge;
use crate::{worker, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditRootAttestation {
    /// 32-byte Merkle root as lowercase hex (the only field that ends
    /// up on-chain, as `metadata_hash`). The worker rejects anything
    /// that is not exactly 32 bytes.
    pub root_hash_hex: String,
    pub release_target: String,
    pub release_subject: String,
    pub release_scope: String,
    pub recorded_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedAttestation {
    pub attestation_pda: String,
    pub signature: String,
}

impl SapBridge {
    pub async fn publish_audit_root(
        &self,
        attestation: &AuditRootAttestation,
    ) -> Result<PublishedAttestation> {
        self.require_enabled()?;
        worker::invoke(self.config(), "attest-root", attestation).await
    }
}
