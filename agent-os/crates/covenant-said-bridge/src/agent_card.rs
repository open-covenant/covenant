//! Off-chain AgentCard registration against SAID's REST API.
//!
//! Free, instant, no SOL. Use this for the MVP path: register every
//! Covenant agent off-chain at boot, then upgrade to on-chain via the
//! worker once a paid-tx gate is opened.

use serde::{Deserialize, Serialize};

use crate::client::SaidBridge;
use crate::rest;
use crate::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCard {
    pub wallet: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OffChainRegistration {
    pub wallet: String,
    pub off_chain: bool,
    #[serde(default)]
    pub created_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentLookup {
    pub wallet: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub is_verified: bool,
    #[serde(default)]
    pub verification_tier: u8,
    #[serde(default)]
    pub stake_amount: u64,
    #[serde(default)]
    pub reputation_score: u16,
    #[serde(default)]
    pub total_interactions: u64,
}

impl SaidBridge {
    /// Register an AgentCard off-chain via REST. No SOL spent. Idempotent
    /// by wallet on SAID's side.
    pub async fn register_off_chain(&self, card: &AgentCard) -> Result<OffChainRegistration> {
        self.require_enabled()?;
        let client = rest::build_client(self.config().rest_timeout)?;
        rest::post_json(&client, &self.config().api_base_url, "/api/agents", card).await
    }

    /// Look up an agent by Solana wallet address. Returns identity +
    /// verification + reputation summary in one fetch.
    pub async fn lookup(&self, wallet: &str) -> Result<AgentLookup> {
        self.require_enabled()?;
        let client = rest::build_client(self.config().rest_timeout)?;
        let path = format!("/api/agents/{wallet}");
        rest::get_json(&client, &self.config().api_base_url, &path).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_card_serializes_camel_case() {
        let card = AgentCard {
            wallet: "AdChc…".into(),
            name: "Covenant".into(),
            description: Some("Open agent coordination".into()),
            metadata_uri: Some("https://opencovenant.org/agent.json".into()),
            homepage: None,
            capabilities: vec!["code.review".into(), "code.write".into()],
            tags: vec![],
        };
        let json = serde_json::to_value(&card).unwrap();
        assert_eq!(json["metadataUri"], "https://opencovenant.org/agent.json");
        assert_eq!(json["capabilities"][0], "code.review");
        assert!(json.get("homepage").is_none());
        assert!(json.get("tags").is_none());
    }
}
