//! Peer lookup via SAP's discovery registry.
//!
//! Skeleton. The follow-up commit decodes SAP discovery accounts and
//! exposes them as `PeerRecord`s that slot into Covenant's peer
//! registry alongside locally-known peers.

use serde::{Deserialize, Serialize};

use crate::client::SapBridge;
use crate::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerRecord {
    pub agent_pda: String,
    pub display: String,
    pub protocols: Vec<String>,
    pub reputation_score: Option<u32>,
}

impl SapBridge {
    pub async fn find_agents_by_protocol(&self, _protocol: &str) -> Result<Vec<PeerRecord>> {
        self.require_enabled()?;
        todo!("decode DiscoveryRegistry by protocol index")
    }

    pub async fn find_agent_by_pda(&self, _pda: &str) -> Result<Option<PeerRecord>> {
        self.require_enabled()?;
        todo!("fetch + decode AgentAccount")
    }
}
