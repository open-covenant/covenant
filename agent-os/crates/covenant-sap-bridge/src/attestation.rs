//! Anchor audit roots into SAP.
//!
//! Only the 32-byte Merkle root and a small structured envelope go
//! on-chain — never the underlying audit-log contents. The root is
//! appended to a self-anchored SAP ledger (the daemon signs for its own
//! agent): SAP rejects self-attestation by design, so the ledger module
//! is the intended path for a single-party audit trail. The ledger PDA
//! is the public, append-only record external parties follow.

use serde::{Deserialize, Serialize};

use crate::client::SapBridge;
use crate::{worker, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditRootAttestation {
    /// 32-byte Merkle root as lowercase hex (goes on-chain as the
    /// ledger entry's `content_hash`). The worker rejects anything that
    /// is not exactly 32 bytes.
    pub root_hash_hex: String,
    pub release_target: String,
    pub release_subject: String,
    pub release_scope: String,
    pub recorded_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedAuditRoot {
    /// The ledger PDA the root was appended to. Stable across roots, so
    /// it doubles as the public handle to the daemon's audit trail.
    pub ledger_pda: String,
    pub signature: String,
}

impl SapBridge {
    pub async fn publish_audit_root(
        &self,
        attestation: &AuditRootAttestation,
    ) -> Result<PublishedAuditRoot> {
        self.require_enabled()?;
        worker::invoke(self.config(), "attest-root", attestation).await
    }
}
