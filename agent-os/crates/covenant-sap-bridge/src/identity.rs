//! Publish + reconcile the daemon's identity as a SAP agent account.
//!
//! [`AgentManifest`] mirrors the arguments SAP's `register_agent` and
//! `update_agent` instructions take. Publishing and updating both
//! drive the TS bridge worker, which builds, signs, and submits the
//! transaction. Reconciliation reads the on-chain account through the
//! worker's `describe-agent` command and produces a [`ManifestDiff`]
//! the caller can act on.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::client::SapBridge;
use crate::{worker, Result};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// Full agent projection — the read used by reconcile / diff paths.
/// Mirrors the worker's `describe-agent` envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDetail {
    pub agent_pda: String,
    pub wallet: String,
    pub name: String,
    pub description: String,
    pub capabilities: Vec<CapabilityDescriptor>,
    /// SDK-native pricing tiers, kept opaque on the Rust side until
    /// the bridge maps Covenant rate cards both ways.
    pub pricing: Vec<serde_json::Value>,
    pub protocols: Vec<String>,
    pub agent_id: Option<String>,
    pub agent_uri: Option<String>,
    pub x402_endpoint: Option<String>,
    pub is_active: bool,
    pub reputation_score: Option<u32>,
}

impl SapBridge {
    /// Publish a fresh agent account on SAP. No-op when the bridge is
    /// disabled — caller decides whether that is a soft warning or a
    /// hard error.
    pub async fn publish_agent(&self, manifest: &AgentManifest) -> Result<PublishedAgent> {
        self.require_enabled()?;
        worker::invoke(self.config(), "publish-agent", manifest).await
    }

    /// Update the daemon's on-chain agent account so it mirrors the
    /// local manifest. Every field is sent non-null — the manifest is
    /// the source of truth.
    pub async fn update_agent(&self, manifest: &AgentManifest) -> Result<PublishedAgent> {
        self.require_enabled()?;
        worker::invoke(self.config(), "update-agent", manifest).await
    }

    /// Fetch the full on-chain agent projection. Returns `None` when
    /// the account doesn't exist (caller should treat as "not yet
    /// published").
    pub async fn describe_agent(&self, pda: &str) -> Result<Option<AgentDetail>> {
        self.require_enabled()?;
        worker::invoke(self.config(), "describe-agent", &PdaQuery { pda }).await
    }

    /// Compute the diff between the on-chain agent at `pda` and the
    /// local `manifest`. Returns `None` when the agent doesn't exist
    /// (publish, don't update). An empty diff means the on-chain
    /// account already matches.
    pub async fn diff_agent(
        &self,
        pda: &str,
        manifest: &AgentManifest,
    ) -> Result<Option<ManifestDiff>> {
        self.require_enabled()?;
        let Some(current) = self.describe_agent(pda).await? else {
            return Ok(None);
        };
        Ok(Some(ManifestDiff::between(&current, manifest)))
    }
}

#[derive(Serialize)]
struct PdaQuery<'a> {
    pda: &'a str,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManifestDiff {
    pub added_capabilities: Vec<CapabilityDescriptor>,
    pub removed_capabilities: Vec<String>,
    pub pricing_changed: bool,
    pub protocols_changed: bool,
    pub metadata_changed: bool,
}

impl ManifestDiff {
    /// True when on-chain state already matches the manifest — no
    /// `update_agent` call needed.
    pub fn is_empty(&self) -> bool {
        self.added_capabilities.is_empty()
            && self.removed_capabilities.is_empty()
            && !self.pricing_changed
            && !self.protocols_changed
            && !self.metadata_changed
    }

    fn between(current: &AgentDetail, manifest: &AgentManifest) -> Self {
        let current_caps: BTreeSet<&str> =
            current.capabilities.iter().map(|c| c.id.as_str()).collect();
        let manifest_caps: BTreeSet<&str> = manifest
            .capabilities
            .iter()
            .map(|c| c.id.as_str())
            .collect();

        let added_capabilities: Vec<CapabilityDescriptor> = manifest
            .capabilities
            .iter()
            .filter(|c| !current_caps.contains(c.id.as_str()))
            .cloned()
            .collect();
        let removed_capabilities: Vec<String> = current
            .capabilities
            .iter()
            .filter(|c| !manifest_caps.contains(c.id.as_str()))
            .map(|c| c.id.clone())
            .collect();

        let current_protocols: BTreeSet<&str> =
            current.protocols.iter().map(String::as_str).collect();
        let manifest_protocols: BTreeSet<&str> =
            manifest.protocols.iter().map(String::as_str).collect();
        let protocols_changed = current_protocols != manifest_protocols;

        // Pricing comparison is intentionally coarse: both sides flow
        // through serde_json::Value, and Covenant's simple PricingTier
        // doesn't yet round-trip to SDK-native tiers. Treat any
        // pricing on the manifest as a potential change until the
        // budget rate-card mapping lands.
        let pricing_changed = !manifest.pricing.is_empty() || !current.pricing.is_empty();

        let metadata_changed = manifest.name != current.name
            || manifest.description.as_deref().unwrap_or("") != current.description
            || manifest.agent_id != current.agent_id
            || manifest.agent_uri != current.agent_uri
            || manifest.x402_endpoint != current.x402_endpoint;

        Self {
            added_capabilities,
            removed_capabilities,
            pricing_changed,
            protocols_changed,
            metadata_changed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(id: &str) -> CapabilityDescriptor {
        CapabilityDescriptor {
            id: id.into(),
            protocol_id: None,
            version: None,
        }
    }

    fn detail(name: &str, caps: &[&str], protocols: &[&str]) -> AgentDetail {
        AgentDetail {
            agent_pda: "p".into(),
            wallet: "w".into(),
            name: name.into(),
            description: String::new(),
            capabilities: caps.iter().map(|c| cap(c)).collect(),
            pricing: vec![],
            protocols: protocols.iter().map(|p| (*p).to_string()).collect(),
            agent_id: None,
            agent_uri: None,
            x402_endpoint: None,
            is_active: true,
            reputation_score: None,
        }
    }

    fn manifest(name: &str, caps: &[&str], protocols: &[&str]) -> AgentManifest {
        AgentManifest {
            name: name.into(),
            capabilities: caps.iter().map(|c| cap(c)).collect(),
            protocols: protocols.iter().map(|p| (*p).to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn identical_manifest_and_chain_yields_empty_diff() {
        let d = ManifestDiff::between(
            &detail("demo", &["a", "b"], &["a2a"]),
            &manifest("demo", &["a", "b"], &["a2a"]),
        );
        assert!(d.is_empty(), "{d:?}");
    }

    #[test]
    fn capability_diff_lists_adds_and_removes() {
        let d = ManifestDiff::between(
            &detail("demo", &["a", "b"], &[]),
            &manifest("demo", &["b", "c"], &[]),
        );
        assert_eq!(d.added_capabilities.len(), 1);
        assert_eq!(d.added_capabilities[0].id, "c");
        assert_eq!(d.removed_capabilities, vec!["a".to_string()]);
        assert!(!d.protocols_changed);
        assert!(!d.metadata_changed);
    }

    #[test]
    fn protocols_set_difference_is_detected() {
        let d = ManifestDiff::between(
            &detail("demo", &[], &["a2a"]),
            &manifest("demo", &[], &["a2a", "x402"]),
        );
        assert!(d.protocols_changed);
    }

    #[test]
    fn rename_marks_metadata_changed() {
        let d = ManifestDiff::between(&detail("old", &[], &[]), &manifest("new", &[], &[]));
        assert!(d.metadata_changed);
    }
}
