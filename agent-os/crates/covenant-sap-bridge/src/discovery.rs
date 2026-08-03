//! Peer lookup via SAP's discovery registry.
//!
//! Resolves peers through the worker, which decodes SAP agent and
//! protocol-index accounts. Results slot into Covenant's peer registry
//! alongside locally-known peers.

use serde::{Deserialize, Serialize};

use crate::client::SapBridge;
use crate::{worker, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerRecord {
    pub agent_pda: String,
    pub display: String,
    pub protocols: Vec<String>,
    /// SAP's own on-chain peer score, decoded by the worker. The scale is
    /// defined upstream by SAP, not by Covenant, and nothing here audits how
    /// it was produced — treat it as discovery metadata, never as an input to
    /// a trust decision. Distinct from `covenant-audit`'s compliance score
    /// and covenantd's escrow standing.
    #[serde(rename = "reputationScore")]
    pub sap_reputation_score: Option<u32>,
}

#[derive(Serialize)]
struct ProtocolQuery<'a> {
    protocol: &'a str,
}

#[derive(Serialize)]
struct PdaQuery<'a> {
    pda: &'a str,
}

impl SapBridge {
    pub async fn find_agents_by_protocol(&self, protocol: &str) -> Result<Vec<PeerRecord>> {
        self.require_enabled()?;
        worker::invoke(
            self.config(),
            "find-by-protocol",
            &ProtocolQuery { protocol },
        )
        .await
    }

    pub async fn find_agent_by_pda(&self, pda: &str) -> Result<Option<PeerRecord>> {
        self.require_enabled()?;
        worker::invoke(self.config(), "find-agent", &PdaQuery { pda }).await
    }
}
