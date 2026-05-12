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
    fn capability_serde_pins_five_field_wire_form() {
        // Capability is the inner payload of SignedCapability and the
        // read shape every JSONL grant log line, IPC RecentCapabilities
        // response, and HTTP grants list response destructures on.
        //
        // * subject / action / scope / granted_by are strictly required.
        // * expires_at: Option<u64> carries #[serde(default)] with no
        //   skip_serializing_if. The serialize side always emits the
        //   key (null on the perpetual-grant path); the deserialize
        //   side accepts missing expires_at and decodes as None.
        //
        // The signed_capability_round_trips_through_serde test pins the
        // outer envelope; this test pins the inner contract directly.

        let subject = AgentId::new("research@local", [7u8; 32]);
        let granted_by = AgentId::new("authority@local", [11u8; 32]);
        let perpetual = Capability {
            subject: subject.clone(),
            action: "memory.write".into(),
            scope: serde_json::json!({"tier": "working"}),
            granted_by: granted_by.clone(),
            expires_at: None,
        };
        let wire = serde_json::to_value(&perpetual).unwrap();
        let obj = wire
            .as_object()
            .expect("Capability serialises as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["action", "expires_at", "granted_by", "scope", "subject"],
            "Capability wire object must always carry the five documented \
             fields; adding skip_serializing_if to expires_at would silently \
             drop the key for perpetual grants and break consumers \
             destructuring on the five-key shape"
        );
        assert_eq!(
            obj.get("expires_at"),
            Some(&serde_json::Value::Null),
            "Capability with expires_at=None must surface expires_at as JSON \
             null on the wire — the absence of skip_serializing_if is what \
             keeps the five-key shape stable across perpetual and \
             time-bounded grants"
        );
        assert_eq!(
            obj.get("action").and_then(serde_json::Value::as_str),
            Some("memory.write"),
            "action must surface verbatim as a JSON string — capability \
             enforcement matches on this exact dotted-path key"
        );
        assert_eq!(
            obj.get("scope"),
            Some(&serde_json::json!({"tier": "working"})),
            "scope must surface as a serde_json::Value pass-through — the \
             per-action scope shape (memory tier, audit before_ms, tool \
             arguments-allow, etc.) leans on this verbatim round-trip"
        );

        let decoded: Capability = serde_json::from_value(wire).unwrap();
        assert_eq!(
            decoded, perpetual,
            "Capability must round-trip through serde_json verbatim — the \
             PartialEq derive is the contract every JSONL replay leans on"
        );

        let timed = Capability {
            subject: subject.clone(),
            action: "memory.write".into(),
            scope: serde_json::json!({"tier": "working"}),
            granted_by: granted_by.clone(),
            expires_at: Some(123_456),
        };
        let timed_wire = serde_json::to_value(&timed).unwrap();
        assert_eq!(
            timed_wire
                .get("expires_at")
                .and_then(serde_json::Value::as_u64),
            Some(123_456),
            "populated expires_at must round-trip verbatim on the wire"
        );

        let no_expires_at = serde_json::json!({
            "subject": {
                "display": subject.display,
                "pubkey": subject.pubkey_base58(),
            },
            "action": "memory.write",
            "scope": {"tier": "working"},
            "granted_by": {
                "display": granted_by.display,
                "pubkey": granted_by.pubkey_base58(),
            },
        });
        let decoded: Capability = serde_json::from_value(no_expires_at).unwrap();
        assert_eq!(
            decoded.expires_at, None,
            "Capability with expires_at omitted must decode as None — the \
             #[serde(default)] forward-compat contract every stale CLI built \
             before the field landed leans on"
        );

        let full_obj = serde_json::to_value(&perpetual).unwrap();
        let full_map = full_obj.as_object().unwrap().clone();
        for required in ["subject", "action", "scope", "granted_by"] {
            let mut payload = full_map.clone();
            payload.remove(required);
            assert!(
                serde_json::from_value::<Capability>(serde_json::Value::Object(payload)).is_err(),
                "Capability must reject a wire payload that omits {required}; \
                 a stray #[serde(default)] introduction on any of the four \
                 strictly required fields would silently let a malformed \
                 grant decode and break capability enforcement at the IPC \
                 boundary"
            );
        }
    }

    #[test]
    fn intent_serde_pins_six_field_wire_form() {
        // Intent is the load-bearing dispatch envelope:
        // covenantd::Server::dispatch_intent, the router, the audit log,
        // and the budget ledger all destructure on its six fields.
        //
        // * id / text / issuer / issued_at are strictly required.
        // * priority carries #[serde(default)] with no
        //   #[serde(skip_serializing_if)]. The serialize side always
        //   emits the key; missing on decode falls back to the Default
        //   impl (Priority::Normal, marked with #[default]).
        // * parent carries #[serde(default)] (redundant for Option<T>
        //   but documents the contract); always emits the key on
        //   serialize, decodes as None when missing.
        //
        // priority serialises in lowercase due to
        // #[serde(rename_all = "lowercase")] on the enum.

        let intent = Intent {
            id: Uuid::from_u128(0x42),
            text: "ship the slice".into(),
            issuer: dummy_id(),
            issued_at: 1_000,
            priority: Priority::High,
            parent: None,
        };
        let wire = serde_json::to_value(&intent).unwrap();
        let obj = wire
            .as_object()
            .expect("Intent serialises as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["id", "issued_at", "issuer", "parent", "priority", "text"],
            "Intent wire object must always carry the six documented fields; \
             adding skip_serializing_if to parent or priority would silently \
             drop the key for the None/default path and break consumers \
             destructuring on Intent shape"
        );
        assert_eq!(
            obj.get("parent"),
            Some(&serde_json::Value::Null),
            "Intent with parent=None must surface parent as JSON null on the \
             wire — the absence of skip_serializing_if keeps the six-key shape \
             stable across delegated and top-level intents"
        );
        assert_eq!(
            obj.get("priority").and_then(serde_json::Value::as_str),
            Some("high"),
            "Priority must serialise as the lowercase string \"high\" — \
             dropping #[serde(rename_all = \"lowercase\")] would silently \
             shift every persisted intent's priority wire form to a \
             different case"
        );

        let decoded: Intent = serde_json::from_value(wire).unwrap();
        assert_eq!(
            decoded, intent,
            "Intent must round-trip through serde_json verbatim — the Eq \
             derive is the contract every dispatch consumer leans on"
        );

        let minimal = serde_json::json!({
            "id": Uuid::from_u128(0x42).to_string(),
            "text": "ship the slice",
            "issuer": {
                "display": dummy_id().display,
                "pubkey": dummy_id().pubkey_base58(),
            },
            "issued_at": 1_000,
        });
        let decoded: Intent = serde_json::from_value(minimal).unwrap();
        assert_eq!(
            decoded.priority,
            Priority::Normal,
            "Intent with priority omitted must decode as Priority::Normal — \
             the #[default] arm on the Priority enum is the forward-compat \
             contract every stale CLI built before the priority field landed \
             relies on"
        );
        assert_eq!(
            decoded.parent, None,
            "Intent with parent omitted must decode as None — serde's \
             auto-default for Option<Uuid> is the forward-compat contract \
             every stale CLI relies on"
        );

        let full_obj = serde_json::to_value(&intent).unwrap();
        let full_map = full_obj.as_object().unwrap().clone();
        for required in ["id", "text", "issuer", "issued_at"] {
            let mut payload = full_map.clone();
            payload.remove(required);
            assert!(
                serde_json::from_value::<Intent>(serde_json::Value::Object(payload)).is_err(),
                "Intent must reject a wire payload that omits {required}; a \
                 stray #[serde(default)] introduction on any of the strictly \
                 required fields would silently let a malformed dispatch \
                 envelope decode at the IPC boundary"
            );
        }

        let bad_case = serde_json::json!({
            "id": Uuid::from_u128(0x42).to_string(),
            "text": "ship the slice",
            "issuer": {
                "display": dummy_id().display,
                "pubkey": dummy_id().pubkey_base58(),
            },
            "issued_at": 1_000,
            "priority": "Normal",
        });
        assert!(
            serde_json::from_value::<Intent>(bad_case).is_err(),
            "Capitalised priority (\"Normal\") must be rejected — the \
             rename_all = lowercase contract is what keeps every persisted \
             intent's priority wire form stable across rebuilds"
        );
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
    fn memory_repair_command_serde_pins_each_snake_case_action_slug() {
        // MemoryRepairCommand carries #[serde(tag = "action", rename_all =
        // "snake_case")] and rides inside every MemoryRepairRequest
        // dispatched through daemon, HTTP, and CLI repair flows. The
        // discriminator name "action" is load-bearing — a refactor to
        // the serde default tag "type" would silently break every
        // CLI-built repair payload still keying on action. The
        // rename_all attribute is similarly load-bearing — its drop
        // would emit titlecase variants in the wire JSON while sibling
        // MemoryRepairAction (already pinned) kept its snake_case
        // slugs, splitting repair audit consumers across two forms.
        // Pin both the tag name and the per-variant snake_case slug,
        // exercising the #[serde(default, skip_serializing_if =
        // "Option::is_none")] expected_parent field on both arms.
        let detach_id = Uuid::new_v4();
        let parent = Uuid::new_v4();
        let delete_id = Uuid::new_v4();
        let backfill_id = Uuid::new_v4();

        let detach_no_parent = MemoryRepairCommand::DetachParent {
            id: detach_id,
            expected_parent: None,
        };
        let detach_no_parent_wire = serde_json::json!({
            "action": "detach_parent",
            "id": detach_id,
        });
        assert_eq!(
            serde_json::to_value(&detach_no_parent).unwrap(),
            detach_no_parent_wire,
        );
        assert_eq!(
            serde_json::from_value::<MemoryRepairCommand>(detach_no_parent_wire).unwrap(),
            detach_no_parent,
        );

        let detach_with_parent = MemoryRepairCommand::DetachParent {
            id: detach_id,
            expected_parent: Some(parent),
        };
        let detach_with_parent_wire = serde_json::json!({
            "action": "detach_parent",
            "id": detach_id,
            "expected_parent": parent,
        });
        assert_eq!(
            serde_json::to_value(&detach_with_parent).unwrap(),
            detach_with_parent_wire,
        );
        assert_eq!(
            serde_json::from_value::<MemoryRepairCommand>(detach_with_parent_wire).unwrap(),
            detach_with_parent,
        );

        let delete = MemoryRepairCommand::DeleteRecord { id: delete_id };
        let delete_wire = serde_json::json!({
            "action": "delete_record",
            "id": delete_id,
        });
        assert_eq!(serde_json::to_value(&delete).unwrap(), delete_wire);
        assert_eq!(
            serde_json::from_value::<MemoryRepairCommand>(delete_wire).unwrap(),
            delete,
        );

        let backfill = MemoryRepairCommand::BackfillProvenance {
            id: backfill_id,
            provenance: serde_json::json!({"source": "manual"}),
        };
        let backfill_wire = serde_json::json!({
            "action": "backfill_provenance",
            "id": backfill_id,
            "provenance": {"source": "manual"},
        });
        assert_eq!(serde_json::to_value(&backfill).unwrap(), backfill_wire);
        assert_eq!(
            serde_json::from_value::<MemoryRepairCommand>(backfill_wire).unwrap(),
            backfill,
        );

        // Dropping rename_all would surface variant names verbatim
        // (DetachParent); the snake_case whitelist must reject that
        // form so the regression fails loud.
        assert!(
            serde_json::from_value::<MemoryRepairCommand>(serde_json::json!({
                "action": "DetachParent",
                "id": detach_id,
            }))
            .is_err(),
            "titlecase action slug (the rename_all default) must be rejected",
        );

        // Switching the tag from "action" to the serde default "type"
        // would silently break every CLI repair payload. Pin the tag
        // name so a refactor that drops tag = "action" fails loud at
        // the boundary instead of through a confusing upstream error.
        assert!(
            serde_json::from_value::<MemoryRepairCommand>(serde_json::json!({
                "type": "detach_parent",
                "id": detach_id,
            }))
            .is_err(),
            "wrong discriminator name (serde default 'type') must be rejected",
        );

        // kebab-case must also fail — the contract is snake_case only,
        // not "any non-titlecase form".
        assert!(
            serde_json::from_value::<MemoryRepairCommand>(serde_json::json!({
                "action": "delete-record",
                "id": delete_id,
            }))
            .is_err(),
            "kebab-case action slug must be rejected",
        );
    }

    #[test]
    fn priority_serde_pins_lowercase_wire_form_and_default() {
        // Priority::Normal must remain the Default arm. Moving #[default]
        // to Low or High would silently shift every default-priority
        // Intent's queueing behavior with no compile-time signal.
        assert_eq!(Priority::default(), Priority::Normal);

        // Lowercase slugs are the wire form for Intent JSON and the
        // agent.toml settlement section. A titlecase regression would
        // break every manifest with priority = high.
        let cases: [(Priority, &str); 3] = [
            (Priority::Low, "low"),
            (Priority::Normal, "normal"),
            (Priority::High, "high"),
        ];
        for (variant, slug) in cases {
            let wire = serde_json::to_string(&variant).unwrap();
            assert_eq!(wire, format!("\"{slug}\""));
            let back: Priority = serde_json::from_str(&wire).unwrap();
            assert_eq!(back, variant);
        }

        assert!(serde_json::from_str::<Priority>("\"Normal\"").is_err());
    }

    #[test]
    fn resource_kind_serde_pins_lowercase_wire_form() {
        // ResourceKind's lowercase slug is the discriminator that flows
        // through SettlementReceipt JSON into the migration planner
        // filter (receipt_migration_plan_json) and the chain-audit grep.
        // Renaming a slug without a migration would silently split the
        // audit pipeline across two forms.
        let cases: [(ResourceKind, &str); 5] = [
            (ResourceKind::Compute, "compute"),
            (ResourceKind::Memory, "memory"),
            (ResourceKind::Tool, "tool"),
            (ResourceKind::Message, "message"),
            (ResourceKind::Registration, "registration"),
        ];
        for (variant, slug) in cases {
            let wire = serde_json::to_string(&variant).unwrap();
            assert_eq!(wire, format!("\"{slug}\""));
            let back: ResourceKind = serde_json::from_str(&wire).unwrap();
            assert_eq!(back, variant);
        }

        assert!(serde_json::from_str::<ResourceKind>("\"Memory\"").is_err());
        assert!(serde_json::from_str::<ResourceKind>("\"memory_record\"").is_err());
    }

    #[test]
    fn budget_pause_reason_serde_pins_snake_case_wire_form() {
        // BudgetPauseReason rides inside BudgetPauseCheckpoint, which the
        // daemon persists through JsonlPauseCheckpointStore. The slugs
        // are durable on disk — renaming one without a migration would
        // silently strand previously paused intents because the resume
        // claim path can't deserialize them.
        let cases: [(BudgetPauseReason, &str); 4] = [
            (BudgetPauseReason::BudgetExhausted, "budget_exhausted"),
            (BudgetPauseReason::OperatorRequested, "operator_requested"),
            (BudgetPauseReason::Shutdown, "shutdown"),
            (BudgetPauseReason::Maintenance, "maintenance"),
        ];
        for (variant, slug) in cases {
            let wire = serde_json::to_string(&variant).unwrap();
            assert_eq!(
                wire,
                format!("\"{slug}\""),
                "{variant:?} must serialize to {slug:?}; a slug rename strands paused checkpoints written by older daemons",
            );
            let back: BudgetPauseReason = serde_json::from_str(&wire).unwrap();
            assert_eq!(back, variant);
        }

        // Snake_case is the only accepted casing — a permissive fallback
        // would silently mask a stale or mis-cased checkpoint.
        assert!(serde_json::from_str::<BudgetPauseReason>("\"BudgetExhausted\"").is_err());
        assert!(serde_json::from_str::<BudgetPauseReason>("\"budget-exhausted\"").is_err());
    }

    #[test]
    fn budget_pause_checkpoint_version_pins_one_and_resume_state_skips_empty() {
        // BudgetPauseCheckpoint::VERSION is the on-disk schema version
        // validated by covenant_budget::validate_pause_checkpoint. A
        // refactor that bumps the const without writing a migration
        // silently strands every previously persisted checkpoint —
        // operators see "cannot resume" errors instead of a clear
        // migration prompt. Pin the const to its concrete value here
        // so any version bump must update this test in the same change
        // and prompt the migration question.
        assert_eq!(
            BudgetPauseCheckpoint::VERSION,
            1,
            "BudgetPauseCheckpoint::VERSION must remain 1 until a migration is written; a silent bump strands every persisted v1 checkpoint",
        );

        let base = BudgetPauseCheckpoint {
            version: BudgetPauseCheckpoint::VERSION,
            intent_id: Uuid::nil(),
            agent: dummy_id(),
            reason: BudgetPauseReason::BudgetExhausted,
            requested_credits: 1,
            tokens_remaining: 0,
            refill_eta_ms: 0,
            saved_at_ms: 0,
            resume_state: serde_json::Map::new(),
        };

        // skip_serializing_if = is_empty must drop the resume_state key
        // when the map is empty; otherwise the on-disk JSONL log grows
        // every checkpoint with a redundant "resume_state":{} and any
        // tooling that grep-matches on the field starts spuriously
        // matching empty rows.
        let empty_wire = serde_json::to_value(&base).unwrap();
        let keys: Vec<&str> = empty_wire
            .as_object()
            .expect("checkpoint must serialize to a JSON object")
            .keys()
            .map(|s| s.as_str())
            .collect();
        assert!(
            !keys.contains(&"resume_state"),
            "empty resume_state must be skipped on serialize so on-disk JSONL stays compact; got keys: {keys:?}",
        );

        // Populating resume_state must surface the key with the
        // inserted value — the skip predicate is empty-only, not "drop
        // resume_state always".
        let mut populated = base.clone();
        populated
            .resume_state
            .insert("k".into(), serde_json::Value::String("v".into()));
        let populated_wire = serde_json::to_value(&populated).unwrap();
        assert_eq!(
            populated_wire
                .as_object()
                .and_then(|o| o.get("resume_state"))
                .and_then(|v| v.as_object())
                .and_then(|m| m.get("k"))
                .and_then(|v| v.as_str()),
            Some("v"),
            "non-empty resume_state must serialize with the inserted entries; otherwise the skip predicate is hiding live state",
        );

        // #[serde(default)] on resume_state must let a JSONL row that
        // omits the field (e.g. a row written before resume_state was
        // added) decode without error. Round-trip the no-resume_state
        // wire form through serde_json to pin the decode path.
        let no_resume_state = serde_json::to_string(&base).unwrap();
        let back: BudgetPauseCheckpoint = serde_json::from_str(&no_resume_state).unwrap();
        assert!(
            back.resume_state.is_empty(),
            "checkpoint decoded from a payload that omits resume_state must have an empty map; the #[serde(default)] arm must remain in place",
        );
        assert_eq!(back.version, BudgetPauseCheckpoint::VERSION);
    }

    #[test]
    fn memory_repair_action_serde_pins_snake_case_wire_form() {
        // MemoryRepairAction is the action discriminator on every
        // MemoryRepairOutcome audit row emitted by daemon, HTTP, and
        // CLI memory repair commands. All three slugs land in audit
        // JSON keyed on the snake_case form; the rename_all default
        // would emit them titlecased and silently bisect repair
        // dashboards.
        let cases: [(MemoryRepairAction, &str); 3] = [
            (MemoryRepairAction::DetachParent, "detach_parent"),
            (MemoryRepairAction::DeleteRecord, "delete_record"),
            (MemoryRepairAction::BackfillProvenance, "backfill_provenance"),
        ];
        for (variant, slug) in cases {
            let wire = serde_json::to_string(&variant).unwrap();
            assert_eq!(wire, format!("\"{slug}\""));
            let back: MemoryRepairAction = serde_json::from_str(&wire).unwrap();
            assert_eq!(back, variant);
        }

        assert!(
            serde_json::from_str::<MemoryRepairAction>("\"DetachParent\"").is_err(),
            "titlecase DetachParent (the rename_all default) must be rejected",
        );
        assert!(
            serde_json::from_str::<MemoryRepairAction>("\"detach-parent\"").is_err(),
            "kebab-case detach-parent must be rejected so the snake_case whitelist stays tight",
        );
    }

    #[test]
    fn memory_repair_outcome_serde_pins_before_after_skip_empty() {
        // MemoryRepairOutcome is the wire form returned by every daemon,
        // HTTP, and CLI memory-repair command, and the same shape lands as
        // an audit row on disk. before and after each carry
        // #[serde(default, skip_serializing_if = "Option::is_none")] so
        // dry-run audit rows that did not capture a snapshot stay compact
        // and stale CLI parsers that read new-daemon rows omitting the
        // field decode to None instead of failing.
        let issuer = AgentId {
            display: "alice@host".into(),
            pubkey: [3u8; 32],
        };
        let record = MemoryRecord {
            id: Uuid::nil(),
            tier: MemoryTier::Working,
            owner: issuer.clone(),
            text: "captured snapshot".into(),
            embedding: vec![],
            metadata: serde_json::json!({}),
            created_at: 1_700_000_000_000,
            parent: None,
        };

        let none_outcome = MemoryRepairOutcome {
            id: Uuid::nil(),
            action: MemoryRepairAction::DetachParent,
            mode: MemoryRepairMode::DryRun,
            would_change: true,
            changed: false,
            before: None,
            after: None,
        };
        let wire = serde_json::to_value(&none_outcome).unwrap();
        let obj = wire.as_object().expect("wire form must be a JSON object");
        assert!(
            !obj.contains_key("before"),
            "before=None must be skipped on the wire; a dropped skip_serializing_if doubles audit row bytes",
        );
        assert!(
            !obj.contains_key("after"),
            "after=None must be skipped on the wire; a dropped skip_serializing_if doubles audit row bytes",
        );

        let some_outcome = MemoryRepairOutcome {
            id: Uuid::nil(),
            action: MemoryRepairAction::DetachParent,
            mode: MemoryRepairMode::Apply,
            would_change: true,
            changed: true,
            before: Some(record.clone()),
            after: Some(record.clone()),
        };
        let wire = serde_json::to_value(&some_outcome).unwrap();
        let back: MemoryRepairOutcome = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, some_outcome,
            "Some(record) before/after must serialize and round-trip verbatim",
        );
        assert!(
            wire.get("before").and_then(|v| v.as_object()).is_some(),
            "Some(record) before must surface as a JSON object on the wire",
        );
        assert!(
            wire.get("after").and_then(|v| v.as_object()).is_some(),
            "Some(record) after must surface as a JSON object on the wire",
        );

        let omitted: MemoryRepairOutcome = serde_json::from_value(serde_json::json!({
            "id": Uuid::nil(),
            "action": "detach_parent",
            "mode": "apply",
            "would_change": true,
            "changed": false,
        }))
        .expect("stale CLI wire form that omits before/after must decode");
        assert_eq!(omitted.before, None);
        assert_eq!(omitted.after, None);

        let null_form: MemoryRepairOutcome = serde_json::from_value(serde_json::json!({
            "id": Uuid::nil(),
            "action": "detach_parent",
            "mode": "apply",
            "would_change": true,
            "changed": false,
            "before": null,
            "after": null,
        }))
        .expect("explicit-null wire form must decode for older daemons that emitted null instead of skipping");
        assert_eq!(null_form.before, None);
        assert_eq!(null_form.after, None);
    }

    #[test]
    fn memory_repair_mode_serde_pins_snake_case_wire_form() {
        // MemoryRepairMode is the dry_run / apply toggle on every
        // MemoryRepairRequest dispatched through daemon, HTTP, and
        // CLI repair flows. DryRun is load-bearing — without
        // rename_all the slug would emit "DryRun" titlecase and the
        // daemon would reject every operator's intended dry-run plan
        // with an opaque deserialize error.
        let cases: [(MemoryRepairMode, &str); 2] = [
            (MemoryRepairMode::DryRun, "dry_run"),
            (MemoryRepairMode::Apply, "apply"),
        ];
        for (variant, slug) in cases {
            let wire = serde_json::to_string(&variant).unwrap();
            assert_eq!(wire, format!("\"{slug}\""));
            let back: MemoryRepairMode = serde_json::from_str(&wire).unwrap();
            assert_eq!(back, variant);
        }

        assert!(
            serde_json::from_str::<MemoryRepairMode>("\"DryRun\"").is_err(),
            "titlecase DryRun (the rename_all default) must be rejected",
        );
        assert!(
            serde_json::from_str::<MemoryRepairMode>("\"dryRun\"").is_err(),
            "camelCase dryRun must be rejected so the snake_case whitelist stays tight",
        );
    }

    #[test]
    fn memory_tier_serde_pins_canonical_longterm_and_legacy_aliases() {
        // Canonical serialize form: lowercase rename_all + LongTerm
        // collapses to the dotless `longterm` slug. Audit-grep
        // conventions and downstream metrics pin the canonical form,
        // so changing it would silently split buckets across two slugs.
        assert_eq!(serde_json::to_string(&MemoryTier::Working).unwrap(), "\"working\"");
        assert_eq!(serde_json::to_string(&MemoryTier::Episodic).unwrap(), "\"episodic\"");
        assert_eq!(serde_json::to_string(&MemoryTier::LongTerm).unwrap(), "\"longterm\"");

        // Canonical deserialize: each lowercase slug round-trips.
        assert_eq!(
            serde_json::from_str::<MemoryTier>("\"working\"").unwrap(),
            MemoryTier::Working,
        );
        assert_eq!(
            serde_json::from_str::<MemoryTier>("\"episodic\"").unwrap(),
            MemoryTier::Episodic,
        );
        assert_eq!(
            serde_json::from_str::<MemoryTier>("\"longterm\"").unwrap(),
            MemoryTier::LongTerm,
        );

        // Legacy LongTerm aliases must still deserialize so older audit
        // lines and saved JSON written before the rename continue to
        // reopen. Iterate over the explicit alias set so dropping one
        // is loud.
        for legacy in ["long-term", "long_term"] {
            let wire = format!("\"{legacy}\"");
            let parsed: MemoryTier = serde_json::from_str(&wire).unwrap_or_else(|err| {
                panic!("legacy MemoryTier alias {legacy:?} must deserialize, got: {err}")
            });
            assert_eq!(
                parsed,
                MemoryTier::LongTerm,
                "alias {legacy:?} must resolve to MemoryTier::LongTerm",
            );
        }

        // Unknown slugs must fail loud — the contract is an exhaustive
        // whitelist. A future #[serde(other)] arm would silently mask
        // malformed audit lines on reopen.
        assert!(serde_json::from_str::<MemoryTier>("\"Working\"").is_err());
        assert!(serde_json::from_str::<MemoryTier>("\"longterms\"").is_err());
        assert!(serde_json::from_str::<MemoryTier>("\"long term\"").is_err());
    }

    #[test]
    fn memory_compaction_policy_is_empty_pins_four_field_contract() {
        // Default: all four fields inactive, predicate is true.
        assert!(MemoryCompactionPolicy::default().is_empty());

        // Each single-field activation must flip is_empty to false so a
        // future refactor that drops one term from the AND chain (or
        // adds a fifth field without updating is_empty) is loud at
        // validate_compaction_request, not at the SQLite plan path.
        let only_working = MemoryCompactionPolicy {
            delete_working_before_ms: Some(1),
            ..MemoryCompactionPolicy::default()
        };
        assert!(!only_working.is_empty());

        let only_episodic = MemoryCompactionPolicy {
            delete_episodic_before_ms: Some(1),
            ..MemoryCompactionPolicy::default()
        };
        assert!(!only_episodic.is_empty());

        let only_longterm = MemoryCompactionPolicy {
            mark_longterm_stale_before_ms: Some(1),
            ..MemoryCompactionPolicy::default()
        };
        assert!(!only_longterm.is_empty());

        let only_detach = MemoryCompactionPolicy {
            detach_stale_parents: true,
            ..MemoryCompactionPolicy::default()
        };
        assert!(!only_detach.is_empty());

        // All four set: also not empty. The explicit field list (no
        // `..default()` spread) forces a new field added to
        // MemoryCompactionPolicy to either land in this arm or break
        // compilation here, so is_empty cannot grow stale silently.
        let all_set = MemoryCompactionPolicy {
            delete_working_before_ms: Some(1),
            delete_episodic_before_ms: Some(2),
            mark_longterm_stale_before_ms: Some(3),
            detach_stale_parents: true,
            marked_at_ms: Some(4),
        };
        assert!(!all_set.is_empty());
    }

    #[test]
    fn settlement_receipt_memory_record_id_skip_empty_pin() {
        // SettlementReceipt carries an asymmetric serde contract:
        // memory_record_id rides #[serde(default, skip_serializing_if =
        // "Option::is_none")] so non-memory receipts (the common case)
        // stay compact, while the seven other Option chain-metadata
        // fields (chain, cluster, batch_id, merkle_root, tx_sig, slot,
        // confirmed_at, onchain_sig) carry #[serde(default)] only and
        // surface as "field":null when the receipt is unconfirmed —
        // downstream dashboards filter on field presence to distinguish
        // unconfirmed (all-null) from confirmed (all-set) receipts.
        let unconfirmed = SettlementReceipt {
            id: Uuid::nil(),
            payer: dummy_id(),
            resource: ResourceKind::Compute,
            memory_record_id: None,
            credits_consumed: 1,
            settled_at: 0,
            chain: None,
            cluster: None,
            batch_id: None,
            merkle_root: None,
            tx_sig: None,
            slot: None,
            confirmed_at: None,
            onchain_sig: None,
        };
        let wire = serde_json::to_value(&unconfirmed).unwrap();
        let obj = wire.as_object().expect("SettlementReceipt serializes as a JSON object");
        assert!(
            !obj.contains_key("memory_record_id"),
            "memory_record_id=None must be skipped on the wire; a dropped skip_serializing_if inflates every non-memory receipt with an empty correlation field",
        );
        for null_field in [
            "chain",
            "cluster",
            "batch_id",
            "merkle_root",
            "tx_sig",
            "slot",
            "confirmed_at",
            "onchain_sig",
        ] {
            assert_eq!(
                obj.get(null_field),
                Some(&serde_json::Value::Null),
                "{null_field}=None must surface as null on the wire; an unintended skip_serializing_if drops the column for every unconfirmed receipt and breaks dashboards keyed on field presence",
            );
        }

        let memory_id = Uuid::new_v4();
        let memory_receipt = SettlementReceipt {
            memory_record_id: Some(memory_id),
            resource: ResourceKind::Memory,
            ..unconfirmed.clone()
        };
        let wire = serde_json::to_value(&memory_receipt).unwrap();
        assert_eq!(
            wire.get("memory_record_id").and_then(|v| v.as_str()),
            Some(memory_id.to_string().as_str()),
            "memory_record_id=Some(uuid) must surface verbatim on the wire",
        );

        let legacy: SettlementReceipt = serde_json::from_value(serde_json::json!({
            "id": Uuid::nil(),
            "payer": serde_json::to_value(dummy_id()).unwrap(),
            "resource": "compute",
            "credits_consumed": 1,
            "settled_at": 0,
        }))
        .expect("legacy row that omits memory_record_id and the seven chain-metadata fields must decode");
        assert_eq!(
            legacy.memory_record_id, None,
            "omitted memory_record_id must decode to None via #[serde(default)]; a dropped attribute strands every legacy JSONL row",
        );
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
