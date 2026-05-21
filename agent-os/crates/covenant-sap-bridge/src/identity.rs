//! Publish + reconcile the daemon's identity as a SAP agent account.
//!
//! [`AgentManifest`] mirrors the arguments SAP's `register_agent`
//! instruction takes. Publishing drives the TS bridge worker, which
//! builds, signs, and submits the transaction.

use serde::{Deserialize, Serialize};

use crate::client::SapBridge;
use crate::{worker, BridgeError, Result};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentManifest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub capabilities: Vec<CapabilityDescriptor>,
    pub pricing: Vec<PricingTier>,
    pub protocols: Vec<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_uri: Option<String>,
    #[serde(default)]
    pub x402_endpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityDescriptor {
    pub id: String,
    #[serde(default)]
    pub protocol_id: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PricingTier {
    pub id: String,
    pub price_usd_micros: u64,
    pub unit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedAgent {
    pub agent_pda: String,
    pub signature: String,
}

impl SapBridge {
    /// Publish a fresh agent account on SAP. No-op when the bridge is
    /// disabled — caller decides whether that is a soft warning or a
    /// hard error.
    pub async fn publish_agent(&self, manifest: &AgentManifest) -> Result<PublishedAgent> {
        self.require_enabled()?;
        worker::invoke(self.config(), "publish-agent", manifest).await
    }

    /// Reconcile an existing on-chain account against the local
    /// manifest. Returns the diff that would be applied on the next
    /// `publish_agent` call.
    ///
    /// Not yet implemented in the foundation slice — identity publish
    /// and discovery land first; manifest reconciliation follows once
    /// the worker exposes an `update-agent` command.
    pub async fn diff_agent(&self, _manifest: &AgentManifest) -> Result<ManifestDiff> {
        self.require_enabled()?;
        Err(BridgeError::Invalid(
            "diff_agent is not implemented in the foundation slice".into(),
        ))
    }
}

#[derive(Debug, Clone, Default)]
pub struct ManifestDiff {
    pub added_capabilities: Vec<CapabilityDescriptor>,
    pub removed_capabilities: Vec<String>,
    pub pricing_changed: bool,
    pub protocols_changed: bool,
}
