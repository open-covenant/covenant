//! Public-API reads against `api.saidprotocol.com`. Lookup hits
//! `GET /api/agents/:wallet`; SAID indexes agents from on-chain
//! `register_agent`, so there is no off-chain register call.

use serde::{Deserialize, Serialize};

use crate::client::SaidBridge;
use crate::rest;
use crate::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentLookup {
    pub wallet: String,
    #[serde(default)]
    pub pda: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub metadata_uri: Option<String>,
    #[serde(default)]
    pub is_verified: bool,
    #[serde(default)]
    pub sponsored: bool,
    #[serde(default)]
    pub reputation_score: f64,
    #[serde(default)]
    pub feedback_count: u64,
    #[serde(default)]
    pub activity_count: u64,
    #[serde(default)]
    pub registered_at: Option<String>,
}

impl SaidBridge {
    pub async fn lookup(&self, wallet: &str) -> Result<AgentLookup> {
        self.require_enabled()?;
        let client = rest::build_client(self.config().rest_timeout)?;
        let path = format!("/api/agents/{wallet}");
        rest::get_json(&client, &self.config().api_base_url, &path).await
    }
}
