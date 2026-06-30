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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_defaults_sparse_record() {
        // SAID agent records are sparse; only `wallet` is guaranteed, so every
        // other field must fall back to its default instead of failing decode.
        let agent: AgentLookup =
            serde_json::from_value(serde_json::json!({ "wallet": "Wal111" })).unwrap();
        assert_eq!(agent.wallet, "Wal111");
        assert!(agent.pda.is_none());
        assert!(agent.metadata_uri.is_none());
        assert!(agent.registered_at.is_none());
        assert!(!agent.is_verified);
        assert!(!agent.sponsored);
        assert!(agent.reputation_score.abs() < f64::EPSILON);
        assert_eq!(agent.feedback_count, 0);
        assert_eq!(agent.activity_count, 0);
    }

    #[test]
    fn lookup_maps_camel_case_keys() {
        let agent: AgentLookup = serde_json::from_value(serde_json::json!({
            "wallet": "Wal111",
            "metadataUri": "ipfs://meta",
            "isVerified": true,
            "sponsored": true,
            "reputationScore": 4.5,
            "feedbackCount": 12,
            "activityCount": 99,
            "registeredAt": "2026-01-02T03:04:05Z"
        }))
        .unwrap();
        assert_eq!(agent.metadata_uri.as_deref(), Some("ipfs://meta"));
        assert_eq!(agent.registered_at.as_deref(), Some("2026-01-02T03:04:05Z"));
        assert!(agent.is_verified);
        assert!(agent.sponsored);
        assert_eq!(agent.feedback_count, 12);
        assert_eq!(agent.activity_count, 99);
        assert!((agent.reputation_score - 4.5).abs() < f64::EPSILON);
    }
}
