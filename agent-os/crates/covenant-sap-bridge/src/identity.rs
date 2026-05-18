//! Publish + reconcile the daemon's identity as a SAP agent account.
//!
//! Skeleton — no on-chain calls yet. The shape of [`AgentManifest`]
//! tracks the fields SAP's `AgentBuilder.register()` expects, so the
//! follow-up commit only has to wire serialization + tx submission.

use serde::{Deserialize, Serialize};

use crate::client::SapBridge;
use crate::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentManifest {
    pub name: String,
    pub capabilities: Vec<CapabilityDescriptor>,
    pub pricing: Vec<PricingTier>,
    pub protocols: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityDescriptor {
    pub id: String,
    pub protocol_id: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingTier {
    pub id: String,
    pub price_usd_micros: u64,
    pub unit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishedAgent {
    pub agent_pda: String,
    pub signature: String,
}

impl SapBridge {
    /// Publish a fresh agent account on SAP. No-op when the bridge is
    /// disabled — caller decides whether that is a soft warning or a
    /// hard error.
    pub async fn publish_agent(&self, _manifest: &AgentManifest) -> Result<PublishedAgent> {
        self.require_enabled()?;
        todo!("wire to synapse-sap-sdk via the TS bridge worker")
    }

    /// Reconcile an existing on-chain account against the local
    /// manifest. Returns the diff that would be applied on the next
    /// `publish_agent` call.
    pub async fn diff_agent(&self, _manifest: &AgentManifest) -> Result<ManifestDiff> {
        self.require_enabled()?;
        todo!("decode AgentAccount and compute the diff")
    }
}

#[derive(Debug, Clone, Default)]
pub struct ManifestDiff {
    pub added_capabilities: Vec<CapabilityDescriptor>,
    pub removed_capabilities: Vec<String>,
    pub pricing_changed: bool,
    pub protocols_changed: bool,
}
