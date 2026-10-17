//! Portable original signed evidence, separate from coordinator bookkeeping.
use covenant_mcp::Content;
use serde::{Deserialize, Serialize};

use crate::{SignedJobEnvelope, SignedWorkReceipt};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvidenceBundle {
    pub schema_version: u32,
    pub task: AgentJobEvidence,
    /// All linked checks in coordinator-recorded order. Signatures authenticate
    /// individual records, not the completeness of this inventory.
    pub checks: Vec<AgentJobEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentJobEvidence {
    pub envelope: SignedJobEnvelope,
    pub receipt: Option<SignedWorkReceipt>,
    /// Keep every content block, including unknown versioned proof reports.
    pub output: Option<Vec<Content>>,
    pub coordinator: AgentEvidenceMetadata,
}

/// These values are coordinator assertions, not receipt-signed evidence or
/// independent confirmation of an on-chain payment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvidenceMetadata {
    pub assigned_operator: String,
    pub phase: String,
    pub charged_micro_usdc: Option<u64>,
    pub payout: Option<AgentEvidencePayout>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvidencePayout {
    pub amount_micro_usdc: u64,
    pub tx_signature: Option<String>,
    pub memo: Option<String>,
}
