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

    /// Two action-string forms scoped to this identity:
    /// `[<prefix>.<display>, <prefix>.<pubkey_b58>]`. The display form
    /// is the v0 default for ergonomics (matches existing CLI/UI grants);
    /// the pubkey-b58 form is the unforgeable Phase-1+ form that resists
    /// the display-collision attack a second peer enables. A capability
    /// granted under either form satisfies a check that consults the
    /// pair — see `check_capabilities_any_of` in covenantd.
    pub fn scoped_action_alternatives(&self, prefix: &str) -> [String; 2] {
        [
            format!("{prefix}.{}", self.display),
            format!("{prefix}.{}", self.pubkey_base58()),
        ]
    }
}

/// Whitelist for `AgentId.display`: `<local>@<host>` where each side is
/// a non-empty run of `[A-Za-z0-9_.-]`. Returns an error for any other
/// shape so the value can't smuggle whitespace, control bytes, or
/// punctuation into capability strings like `a2a.respond.<sender>`.
///
/// Called automatically from `Deserialize` so every wire-derived
/// `AgentId` (HTTP/IPC requests, agent manifest TOML, A2A tasks)
/// passes through it. Trusted in-process constructions via
/// [`AgentId::new`] are not validated — the test suite and the daemon
/// itself author display strings of known shape.
pub fn validate_agent_id_display(s: &str) -> Result<(), AgentIdError> {
    let (local, host) = s
        .split_once('@')
        .ok_or_else(|| AgentIdError::InvalidDisplay(s.to_owned()))?;
    if local.is_empty() || host.is_empty() {
        return Err(AgentIdError::InvalidDisplay(s.to_owned()));
    }
    if host.contains('@') {
        return Err(AgentIdError::InvalidDisplay(s.to_owned()));
    }
    fn segment_ok(part: &str) -> bool {
        part.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    }
    if !segment_ok(local) || !segment_ok(host) {
        return Err(AgentIdError::InvalidDisplay(s.to_owned()));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AgentIdError {
    #[error("invalid AgentId.display: {0:?}")]
    InvalidDisplay(String),
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
        validate_agent_id_display(&r.display).map_err(serde::de::Error::custom)?;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRepairMode {
    DryRun,
    Apply,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum MemoryRepairCommand {
    DetachParent {
        id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_parent: Option<Uuid>,
    },
    DeleteRecord {
        id: Uuid,
    },
    BackfillProvenance {
        id: Uuid,
        provenance: serde_json::Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryRepairRequest {
    pub mode: MemoryRepairMode,
    pub command: MemoryRepairCommand,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRepairAction {
    DetachParent,
    DeleteRecord,
    BackfillProvenance,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryRepairOutcome {
    pub id: Uuid,
    pub action: MemoryRepairAction,
    pub mode: MemoryRepairMode,
    pub would_change: bool,
    pub changed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<MemoryRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<MemoryRecord>,
}

impl MemoryRepairCommand {
    pub fn id(&self) -> Uuid {
        match self {
            Self::DetachParent { id, .. } => *id,
            Self::DeleteRecord { id } => *id,
            Self::BackfillProvenance { id, .. } => *id,
        }
    }

    pub fn action(&self) -> MemoryRepairAction {
        match self {
            Self::DetachParent { .. } => MemoryRepairAction::DetachParent,
            Self::DeleteRecord { .. } => MemoryRepairAction::DeleteRecord,
            Self::BackfillProvenance { .. } => MemoryRepairAction::BackfillProvenance,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryCompactionPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_working_before_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_episodic_before_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark_longterm_stale_before_ms: Option<u64>,
    #[serde(default)]
    pub detach_stale_parents: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marked_at_ms: Option<u64>,
}

impl MemoryCompactionPolicy {
    pub fn is_empty(&self) -> bool {
        self.delete_working_before_ms.is_none()
            && self.delete_episodic_before_ms.is_none()
            && self.mark_longterm_stale_before_ms.is_none()
            && !self.detach_stale_parents
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryCompactionRequest {
    pub mode: MemoryRepairMode,
    pub policy: MemoryCompactionPolicy,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryCompactionOutcome {
    pub mode: MemoryRepairMode,
    pub would_change: bool,
    pub changed: bool,
    pub deleted: Vec<Uuid>,
    pub stale_marked: Vec<Uuid>,
    pub parents_detached: Vec<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetPauseReason {
    BudgetExhausted,
    OperatorRequested,
    Shutdown,
    Maintenance,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetPauseCheckpoint {
    pub version: u16,
    pub intent_id: Uuid,
    pub agent: AgentId,
    pub reason: BudgetPauseReason,
    pub requested_credits: u64,
    pub tokens_remaining: u64,
    pub refill_eta_ms: u64,
    pub saved_at_ms: u64,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub resume_state: serde_json::Map<String, serde_json::Value>,
}

impl BudgetPauseCheckpoint {
    pub const VERSION: u16 = 1;
}

/// One consumption event recorded by the settlement layer.
///
/// Chain metadata is empty until the receipt is batched and confirmed on
/// Solana. `onchain_sig` remains as a backwards-compatible alias for
/// `tx_sig` while older clients roll forward.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SettlementReceipt {
    pub id: Uuid,
    pub payer: AgentId,
    pub resource: ResourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_record_id: Option<Uuid>,
    /// USD-pegged credits destroyed at this event.
    pub credits_consumed: u64,
    pub settled_at: u64,
    #[serde(default)]
    pub chain: Option<String>,
    #[serde(default)]
    pub cluster: Option<String>,
    #[serde(default)]
    pub batch_id: Option<String>,
    #[serde(default)]
    pub merkle_root: Option<String>,
    #[serde(default)]
    pub tx_sig: Option<String>,
    #[serde(default)]
    pub slot: Option<u64>,
    #[serde(default)]
    pub confirmed_at: Option<u64>,
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
    fn validate_display_accepts_canonical_shapes() {
        for ok in [
            "user@local",
            "research@local",
            "orch-a@host_b",
            "a.b.c@x.y.z",
            "u_1@h-2",
            "A@B",
        ] {
            assert!(
                validate_agent_id_display(ok).is_ok(),
                "expected {ok:?} to validate"
            );
        }
    }

    #[test]
    fn validate_display_rejects_smuggling_shapes() {
        for bad in [
            "",
            "noatsign",
            "@local",
            "user@",
            "user@@local",
            "user@a@b",
            "user@host space",
            "user@local;a2a.respond.victim",
            "user@local\nrest",
            "user@local\trest",
            "user@local/path",
            "user@host:port",
            "user@host?q",
            "user@hős",
        ] {
            assert!(
                validate_agent_id_display(bad).is_err(),
                "expected {bad:?} to fail validation"
            );
        }
    }

    #[test]
    fn agent_id_deserialize_rejects_invalid_display() {
        let valid_pubkey = bs58::encode([0u8; 32]).into_string();
        let bad = format!(r#"{{"display":"a2a.respond.victim;evil","pubkey":"{valid_pubkey}"}}"#);
        let r: Result<AgentId, _> = serde_json::from_str(&bad);
        let err = r.expect_err("should reject");
        assert!(
            err.to_string().contains("invalid AgentId.display"),
            "error should name the failure: {err}"
        );
    }

    #[test]
    fn agent_id_deserialize_accepts_valid_display() {
        let valid_pubkey = bs58::encode([0u8; 32]).into_string();
        let good = format!(r#"{{"display":"orch@local","pubkey":"{valid_pubkey}"}}"#);
        let parsed: AgentId = serde_json::from_str(&good).expect("should parse");
        assert_eq!(parsed.display, "orch@local");
    }

    #[test]
    fn scoped_action_alternatives_emits_display_first() {
        let id = AgentId::new("research@local", [7u8; 32]);
        let pair = id.scoped_action_alternatives("a2a.send");
        assert_eq!(pair[0], "a2a.send.research@local");
        assert!(pair[1].starts_with("a2a.send."));
        assert_ne!(pair[0], pair[1]);
    }

    #[test]
    fn scoped_action_alternatives_b58_uses_pubkey() {
        let id = AgentId::new("orch@local", [7u8; 32]);
        let pair = id.scoped_action_alternatives("a2a.recv");
        let expected_b58 = bs58::encode([7u8; 32]).into_string();
        assert_eq!(pair[1], format!("a2a.recv.{expected_b58}"));
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
            memory_record_id: Some(Uuid::nil()),
            credits_consumed: 42,
            settled_at: 0,
            chain: Some("solana".to_string()),
            cluster: Some("devnet".to_string()),
            batch_id: Some("batch-1".to_string()),
            merkle_root: Some("root".to_string()),
            tx_sig: Some("sig".to_string()),
            slot: Some(7),
            confirmed_at: Some(99),
            onchain_sig: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"onchain_sig\":null"));
        let back: SettlementReceipt = serde_json::from_str(&json).unwrap();
        assert!(back.onchain_sig.is_none());
        assert_eq!(back.memory_record_id, Some(Uuid::nil()));
        assert_eq!(back.credits_consumed, 42);
        assert_eq!(back.chain.as_deref(), Some("solana"));
        assert_eq!(back.tx_sig.as_deref(), Some("sig"));
    }

    #[test]
    fn memory_repair_command_id_and_action_pin_each_variant() {
        let detach_id = Uuid::new_v4();
        let other_parent = Uuid::new_v4();
        let delete_id = Uuid::new_v4();
        let backfill_id = Uuid::new_v4();

        let detach = MemoryRepairCommand::DetachParent {
            id: detach_id,
            expected_parent: Some(other_parent),
        };
        let delete = MemoryRepairCommand::DeleteRecord { id: delete_id };
        let backfill = MemoryRepairCommand::BackfillProvenance {
            id: backfill_id,
            provenance: serde_json::json!({"source": "manual"}),
        };

        // id() returns the target record id, NOT expected_parent or any
        // other nested Uuid. Pin the binding so a refactor that swapped
        // id with expected_parent for DetachParent is loud.
        assert_eq!(detach.id(), detach_id);
        assert_eq!(delete.id(), delete_id);
        assert_eq!(backfill.id(), backfill_id);

        // action() maps each variant to its MemoryRepairAction
        // discriminator. Pin the variant→action correspondence so a
        // refactor that mis-labeled one arm cannot land silently.
        assert_eq!(detach.action(), MemoryRepairAction::DetachParent);
        assert_eq!(delete.action(), MemoryRepairAction::DeleteRecord);
        assert_eq!(backfill.action(), MemoryRepairAction::BackfillProvenance);
    }

    #[test]
    fn settlement_receipt_deserializes_pre_chain_metadata_rows() {
        let pubkey = bs58::encode([7u8; 32]).into_string();
        let json = format!(
            r#"{{
                "id":"{}",
                "payer":{{"display":"research@local","pubkey":"{}"}},
                "resource":"memory",
                "credits_consumed":42,
                "settled_at":0,
                "onchain_sig":null
            }}"#,
            Uuid::nil(),
            pubkey
        );
        let back: SettlementReceipt = serde_json::from_str(&json).unwrap();
        assert_eq!(back.credits_consumed, 42);
        assert_eq!(back.memory_record_id, None);
        assert_eq!(back.chain, None);
        assert_eq!(back.tx_sig, None);
    }
}
