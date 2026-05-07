//! Core data types shared across the Covenant workspace.
//!
//! Defines the wire-level shapes that flow between the daemon, agents,
//! and the storage primitives — [`Intent`], [`AgentId`], [`Priority`],
//! [`MemoryRecord`], [`MemoryTier`], [`Capability`], and
//! [`SettlementReceipt`].

#![deny(unsafe_code)]

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryTier {
    /// Per-task scratchpad. Cleared at task completion.
    Working,
    /// Session-scoped history.
    Episodic,
    /// Persistent knowledge.
    #[serde(rename = "longterm", alias = "long-term", alias = "long_term")]
    LongTerm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceKind {
    Compute,
    Memory,
    Tool,
    Message,
    Registration,
}

/// Identifier for any actor (human, agent, tool, service).
///
/// `display` is the human-readable `name@host` form (CLI, logs, UI).
/// `pubkey` is the 32-byte ed25519 public key — also the Solana settlement key.
/// JSON wire form encodes `pubkey` as base58.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentId {
    pub display: String,
    pub pubkey: [u8; 32],
}

impl AgentId {
    pub fn new(display: impl Into<String>, pubkey: [u8; 32]) -> Self {
        Self {
            display: display.into(),
            pubkey,
        }
    }

    pub fn pubkey_base58(&self) -> String {
        bs58::encode(self.pubkey).into_string()
    }
}

#[derive(Serialize, Deserialize)]
struct AgentIdRepr {
    display: String,
    pubkey: String,
}

impl Serialize for AgentId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        AgentIdRepr {
            display: self.display.clone(),
            pubkey: self.pubkey_base58(),
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for AgentId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let r = AgentIdRepr::deserialize(d)?;
        let bytes = bs58::decode(&r.pubkey)
            .into_vec()
            .map_err(serde::de::Error::custom)?;
        if bytes.len() != 32 {
            return Err(serde::de::Error::custom(format!(
                "pubkey must decode to 32 bytes, got {}",
                bytes.len()
            )));
        }
        let mut pubkey = [0u8; 32];
        pubkey.copy_from_slice(&bytes);
        Ok(AgentId {
            display: r.display,
            pubkey,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Intent {
    pub id: Uuid,
    pub text: String,
    pub issuer: AgentId,
    pub issued_at: u64,
    #[serde(default)]
    pub priority: Priority,
    /// Parent intent for sub-intents delegated by another agent.
    #[serde(default)]
    pub parent: Option<Uuid>,
}

/// A delegated permission. `action` uses dotted paths (e.g. `memory.write`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Capability {
    pub subject: AgentId,
    pub action: String,
    /// Action-specific scope. Shape depends on `action`.
    pub scope: serde_json::Value,
    pub granted_by: AgentId,
    /// Epoch milliseconds; `None` is perpetual.
    #[serde(default)]
    pub expires_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryRecord {
    pub id: Uuid,
    pub tier: MemoryTier,
    pub owner: AgentId,
    pub text: String,
    pub embedding: Vec<f32>,
    pub metadata: serde_json::Value,
    pub created_at: u64,
    /// Parent record for derived memories.
    #[serde(default)]
    pub parent: Option<Uuid>,
}

/// One consumption event recorded by the settlement layer.
///
/// `onchain_sig` is `None` until the receipt is batched and flushed to Solana.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SettlementReceipt {
    pub id: Uuid,
    pub payer: AgentId,
    pub resource: ResourceKind,
    /// USD-pegged credits destroyed at this event.
    pub credits_consumed: u64,
    pub settled_at: u64,
    #[serde(default)]
    pub onchain_sig: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_id() -> AgentId {
        AgentId::new("research@local", [7u8; 32])
    }

    #[test]
    fn agent_id_roundtrip_uses_base58_pubkey() {
        let a = dummy_id();
        let json = serde_json::to_string(&a).unwrap();
        let b: AgentId = serde_json::from_str(&json).unwrap();
        assert_eq!(a, b);
        assert!(json.contains("\"pubkey\""));
        assert!(!json.contains("[7,7"));
    }

    #[test]
    fn agent_id_rejects_wrong_pubkey_length() {
        let bad = r#"{"display":"x@local","pubkey":"deadbeef"}"#;
        let r: Result<AgentId, _> = serde_json::from_str(bad);
        assert!(r.is_err());
    }

    #[test]
    fn intent_uses_default_priority_when_missing() {
        let i = Intent {
            id: Uuid::nil(),
            text: "find recent papers on agent memory".into(),
            issuer: dummy_id(),
            issued_at: 1_700_000_000_000,
            priority: Priority::default(),
            parent: None,
        };
        let json = serde_json::to_string(&i).unwrap();
        let j: Intent = serde_json::from_str(&json).unwrap();
        assert_eq!(j.text, i.text);
        assert_eq!(j.priority, Priority::Normal);
    }

    #[test]
    fn settlement_receipt_serialises_offline_sig_as_null() {
        let r = SettlementReceipt {
            id: Uuid::nil(),
            payer: dummy_id(),
            resource: ResourceKind::Memory,
            credits_consumed: 42,
            settled_at: 0,
            onchain_sig: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"onchain_sig\":null"));
        let back: SettlementReceipt = serde_json::from_str(&json).unwrap();
        assert!(back.onchain_sig.is_none());
        assert_eq!(back.credits_consumed, 42);
    }
}
