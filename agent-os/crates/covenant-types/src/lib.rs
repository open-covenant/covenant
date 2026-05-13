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
    fn validate_agent_id_display_pins_each_rejection_arm() {
        // validate_agent_id_display gates every wire-derived AgentId.display
        // string from HTTP/IPC, agent manifest TOML, and A2A tasks. The
        // validator has seven arms:
        //   1. Missing '@' separator → InvalidDisplay
        //   2. Empty local part ("") → InvalidDisplay
        //   3. Empty host part ("") → InvalidDisplay
        //   4. host.contains('@') (multi-@) → InvalidDisplay
        //   5. local segment_ok with bad char → InvalidDisplay
        //   6. host segment_ok with bad char → InvalidDisplay
        //   7. all-pass → Ok
        //
        // agent_id_deserialize_rejects_invalid_display covers arm 5 (the
        // local-segment invalid-character path via a ';' punctuation
        // test); arms 1, 2, 3, 4, and 6 are not individually pinned.
        // A relaxation of any of these arms — e.g., dropped multi-@
        // guard for forward-compat with email-style displays, accepted
        // empty-host strings, or removed the explicit @ requirement —
        // would silently let a malformed AgentId.display decode through
        // every wire boundary and route through the capability system
        // as if it were a well-formed identity.
        assert_eq!(
            validate_agent_id_display("no-at-separator"),
            Err(AgentIdError::InvalidDisplay("no-at-separator".into())),
            "arm 1: a display string with no '@' separator must be rejected; \
             a refactor that defaulted split_once('@') to Some((s, \"\")) \
             would silently let bare-name displays decode as valid identities \
             with empty hosts",
        );
        assert_eq!(
            validate_agent_id_display("@host"),
            Err(AgentIdError::InvalidDisplay("@host".into())),
            "arm 2: an empty local part must be rejected; a relaxation \
             treating '@host' as syntactic sugar for the daemon identity \
             would mask the routing target on operator audit dashboards",
        );
        assert_eq!(
            validate_agent_id_display("local@"),
            Err(AgentIdError::InvalidDisplay("local@".into())),
            "arm 3: an empty host part must be rejected; a relaxation \
             treating 'local@' as a default-host shorthand would mask \
             the routing target on operator audit dashboards",
        );
        assert_eq!(
            validate_agent_id_display("local@host@extra"),
            Err(AgentIdError::InvalidDisplay("local@host@extra".into())),
            "arm 4: host containing additional '@' must be rejected; a \
             refactor that relaxed this guard for email-style displays \
             would let 'evil@a.com@victim.b.com' decode as a single \
             AgentId.display — operator eyeball-grep treats it as \
             'evil@a.com' but the capability-action whitelist treats it \
             as one opaque identifier (an authority-confusion vector)",
        );
        assert_eq!(
            validate_agent_id_display("local@bad host"),
            Err(AgentIdError::InvalidDisplay("local@bad host".into())),
            "arm 6: host segment with an invalid character (space) must \
             be rejected; segment_ok rejects anything outside \
             [A-Za-z0-9_.-], so a relaxation here would let arbitrary \
             punctuation into the host slot and split the cap-action \
             whitelist's destructuring assumptions",
        );
        validate_agent_id_display("research@local")
            .expect("arm 7: a well-formed 'name@host' display must pass the whitelist");
    }

    #[test]
    fn agent_id_deserialize_accepts_valid_display() {
        let valid_pubkey = bs58::encode([0u8; 32]).into_string();
        let good = format!(r#"{{"display":"orch@local","pubkey":"{valid_pubkey}"}}"#);
        let parsed: AgentId = serde_json::from_str(&good).expect("should parse");
        assert_eq!(parsed.display, "orch@local");
    }

    #[test]
    fn agent_id_serde_pins_two_field_wire_form() {
        // AgentId is the foundational identity envelope that flows into
        // every Capability::subject, Capability::granted_by, Intent::issuer,
        // SettlementReceipt::payer, BudgetDebit::agent, and PeerSummary
        // record. It ships a custom Serialize/Deserialize that maps the
        // [u8; 32] pubkey to a base58 JSON string and validates the
        // display whitelist on every wire-derived AgentId.
        //
        // The existing tests cover round-trip (agent_id_roundtrip_uses_base58_pubkey),
        // pubkey length (agent_id_rejects_wrong_pubkey_length), and display
        // whitelist (agent_id_deserialize_rejects_invalid_display /
        // agent_id_deserialize_accepts_valid_display). None of them pin
        // the exact two-key wire shape, the JSON-string-not-array
        // contract for pubkey, or the per-required-field omission reject —
        // so a refactor that dropped the custom Serialize impl in favour
        // of `derive Serialize` on the [u8; 32] pubkey could land
        // silently, surfacing pubkey as a JSON array of 32 numbers and
        // invalidating every persisted JSONL grant log, A2A mailbox
        // event, audit row, and settlement receipt on operator restart.
        let id = AgentId::new("research@local", [7u8; 32]);

        let wire = serde_json::to_value(&id).unwrap();
        let obj = wire
            .as_object()
            .expect("AgentId serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["display", "pubkey"],
            "AgentId wire form must be exactly two top-level keys \
             ('display', 'pubkey'). A refactor that grew AgentIdRepr a \
             third field without a #[serde(default)] would brick every \
             persisted JSONL grant log line, A2A mailbox event, audit \
             row, and settlement receipt on operator restart"
        );

        assert_eq!(
            obj.get("display"),
            Some(&serde_json::json!("research@local")),
            "AgentId::display must surface as a JSON string equal to \
             the constructor input; a refactor that renamed the field \
             or applied #[serde(rename)] would split capability lookups \
             across two wire forms"
        );

        let pubkey_str = obj
            .get("pubkey")
            .and_then(serde_json::Value::as_str)
            .expect(
                "AgentId::pubkey must surface as a JSON string (custom \
                 Serialize impl); a refactor that dropped the impl in \
                 favour of derived Serialize on [u8; 32] would emit a \
                 JSON array of 32 numbers and invalidate every persisted \
                 grant/event/receipt",
            );
        let expected_b58 = bs58::encode([7u8; 32]).into_string();
        assert_eq!(
            pubkey_str, expected_b58,
            "AgentId::pubkey wire form must equal bs58::encode(pubkey_bytes); \
             a refactor to base64 or hex would silently strand every JSONL \
             grant log line, A2A mailbox event, audit row, and settlement \
             receipt at the daemon's bs58::decode boundary"
        );

        let serialized = serde_json::to_string(&id).unwrap();
        assert!(
            !serialized.contains("[7,7"),
            "serialized AgentId must not contain a raw [u8; 32] byte \
             array; the contract is bs58 string encoding, not derived \
             array Serialize"
        );

        let back: AgentId = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, id,
            "AgentId must round-trip through serde_json verbatim — \
             the PartialEq derive is the contract every wire and JSONL \
             read path leans on"
        );

        let mut missing_display = obj.clone();
        missing_display.remove("display");
        assert!(
            serde_json::from_value::<AgentId>(serde_json::Value::Object(missing_display)).is_err(),
            "AgentId wire form must reject a payload missing 'display'; \
             a regression that gave display a #[serde(default)] would \
             silently substitute an empty string and bypass the \
             validate_agent_id_display whitelist"
        );

        let mut missing_pubkey = obj.clone();
        missing_pubkey.remove("pubkey");
        assert!(
            serde_json::from_value::<AgentId>(serde_json::Value::Object(missing_pubkey)).is_err(),
            "AgentId wire form must reject a payload missing 'pubkey'; \
             a regression that gave pubkey a #[serde(default)] would \
             silently substitute an empty string and decode to a 0-byte \
             buffer that the length check would have to catch downstream"
        );

        let pubkey_as_array = serde_json::json!({
            "display": "research@local",
            "pubkey": [
                7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
                7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7
            ],
        });
        assert!(
            serde_json::from_value::<AgentId>(pubkey_as_array).is_err(),
            "AgentId wire form must reject a pubkey serialised as a JSON \
             array of 32 numbers — the custom deserializer is \
             string-based; accepting the array form would silently \
             accept payloads emitted by a regressed derive-Serialize \
             refactor and let the two encodings diverge in the wild"
        );
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
    fn pubkey_base58_pins_bitcoin_alphabet_leading_zero_ones_and_distinctness() {
        // AgentId::pubkey_base58 (line 63-65) is the canonical
        // base58 projection of the 32-byte pubkey field. It feeds
        // scoped_action_alternatives (line 74) which produces
        // capability-action strings like 'a2a.recv.<pubkey_b58>'
        // that covenant_permissions checks at grant-time and
        // dispatch-time, surfaces directly in operator-facing audit
        // rows, and round-trips into Solana tooling that expects the
        // Bitcoin base58 alphabet (no 0/O/I/l).
        //
        // scoped_action_alternatives_b58_uses_pubkey (above) verifies
        // the COMPOSITION of pubkey_base58 inside the format string,
        // but not the standalone behavior. A refactor that swapped
        // bs58::encode for bs58::encode_check (which appends a 4-byte
        // checksum) would silently widen every audit-row pubkey_b58
        // by ~6 chars and break every persisted capability token's
        // string-equality compare. A refactor that switched to hex
        // would silently double the field length. A refactor that
        // switched the alphabet (e.g., to Ripple) would silently
        // change the char set even when bytes match.

        // (1) Leading-zero handling: base58 with the Bitcoin
        // alphabet maps each leading zero byte to a leading '1'
        // character. An all-zeros pubkey must produce exactly 32
        // leading '1's.
        let zeros = AgentId::new("z@local", [0u8; 32]);
        assert_eq!(
            zeros.pubkey_base58(),
            "1".repeat(32),
            "an all-zeros pubkey must encode to exactly 32 leading \
             '1' characters per the Bitcoin base58 leading-zero \
             convention — a refactor that swapped bs58::encode for \
             bs58::encode_check (which appends a 4-byte checksum) or \
             switched to a hex projection would silently change the \
             length and break every persisted capability token's \
             string-equality compare against the unchecked form",
        );

        // (2) Cross-bind to the bs58 crate behavior on a known
        // pubkey: the function must be exactly bs58::encode of
        // self.pubkey, no transformation, no padding, no checksum.
        let seven = AgentId::new("s@local", [7u8; 32]);
        let expected = bs58::encode([7u8; 32]).into_string();
        assert_eq!(
            seven.pubkey_base58(),
            expected,
            "pubkey_base58 must equal bs58::encode(self.pubkey).into_string() \
             on a known pubkey — pinning that no extra transformation \
             (case-fold, prefix, suffix, checksum) wraps the bs58 call",
        );

        // (3) Distinctness: two distinct pubkeys must produce
        // distinct base58 strings — rules out a 'always returns a
        // constant' regression that would also satisfy the
        // round-trip arms.
        let one = AgentId::new("a@local", [1u8; 32]);
        let two = AgentId::new("b@local", [2u8; 32]);
        assert_ne!(
            one.pubkey_base58(),
            two.pubkey_base58(),
            "two pubkeys with different bytes must produce different \
             base58 strings — a refactor that returned a constant or \
             accidentally read the wrong field would silently collapse \
             every AgentId to one identity in scoped_action_alternatives \
             and every audit-row pubkey_b58 column",
        );

        // (4) Charset: Bitcoin alphabet excludes '0', 'O', 'I', 'l'
        // to avoid visual confusion. Pin that the output never
        // contains these chars on a non-trivial pubkey.
        let charset_check = seven.pubkey_base58();
        assert!(
            charset_check
                .chars()
                .all(|c| !matches!(c, '0' | 'O' | 'I' | 'l')),
            "pubkey_base58 must use the Bitcoin alphabet (no 0, O, I, l) \
             — a refactor that switched to the Ripple alphabet, \
             base64, or hex would let one of these chars surface; got \
             {charset_check:?}",
        );
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
            (
                MemoryRepairAction::BackfillProvenance,
                "backfill_provenance",
            ),
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
    fn memory_repair_outcome_strict_required_fields_reject_on_omission() {
        // MemoryRepairOutcome is the wire form every daemon, HTTP, and CLI
        // memory-repair command returns, and the shape the
        // MemoryRepairApplied audit row destructures. Five strictly
        // required fields with no serde attributes: id, action, mode,
        // would_change, changed. The would_change/changed boolean pair
        // carries the load-bearing dry-run-vs-apply distinction every
        // audit consumer relies on — a #[serde(default)] regression on
        // either would silently collapse that distinction.
        // memory_repair_outcome_serde_pins_before_after_skip_empty (line
        // 1091) pins the before/after skip-empty contract; this test
        // pins the strict-required-fields rejection contract that the
        // existing test does not exercise.
        let id = Uuid::new_v4();
        let compact = MemoryRepairOutcome {
            id,
            action: MemoryRepairAction::DetachParent,
            mode: MemoryRepairMode::DryRun,
            would_change: true,
            changed: false,
            before: None,
            after: None,
        };

        let wire = serde_json::to_value(&compact).unwrap();
        let obj = wire
            .as_object()
            .expect("MemoryRepairOutcome serializes as a JSON object");
        assert_eq!(
            obj.len(),
            5,
            "MemoryRepairOutcome with before=None and after=None must surface exactly five keys on the wire (the skip-empty pair is dropped); a refactor that dropped skip_serializing_if from before or after would inflate every dry-run row",
        );

        for required in ["id", "action", "mode", "would_change", "changed"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<MemoryRepairOutcome>(serde_json::Value::Object(missing))
                    .is_err(),
                "MemoryRepairOutcome wire form must reject a payload missing {required:?}; a stray #[serde(default)] on a required field would silently let a malformed audit row decode (would_change/changed defaulted to false collapses the dry-run-vs-apply distinction)",
            );
        }

        let record = MemoryRecord {
            id: Uuid::nil(),
            tier: MemoryTier::Working,
            owner: dummy_id(),
            text: "captured".into(),
            embedding: vec![],
            metadata: serde_json::json!({}),
            created_at: 1_700_000_000_000,
            parent: None,
        };
        let populated = MemoryRepairOutcome {
            id,
            action: MemoryRepairAction::DetachParent,
            mode: MemoryRepairMode::Apply,
            would_change: true,
            changed: true,
            before: Some(record.clone()),
            after: Some(record),
        };
        let wire = serde_json::to_value(&populated).unwrap();
        assert_eq!(
            wire.as_object().unwrap().len(),
            7,
            "MemoryRepairOutcome with before=Some and after=Some must surface all seven keys on the wire",
        );
        let back: MemoryRepairOutcome = serde_json::from_value(wire).unwrap();
        assert_eq!(
            back, populated,
            "MemoryRepairOutcome must round-trip through serde_json verbatim — the PartialEq derive is the contract every audit consumer joins on",
        );
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
        assert_eq!(
            serde_json::to_string(&MemoryTier::Working).unwrap(),
            "\"working\""
        );
        assert_eq!(
            serde_json::to_string(&MemoryTier::Episodic).unwrap(),
            "\"episodic\""
        );
        assert_eq!(
            serde_json::to_string(&MemoryTier::LongTerm).unwrap(),
            "\"longterm\""
        );

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
    fn memory_compaction_policy_serde_pins_optional_cutoffs_skip_empty_and_bool_always_present() {
        // MemoryCompactionPolicy is the per-tier compaction policy
        // embedded in MemoryCompactionRequest.policy. Five fields:
        // four Option<u64> cutoffs each with #[serde(default,
        // skip_serializing_if = "Option::is_none")] —
        // delete_working_before_ms, delete_episodic_before_ms,
        // mark_longterm_stale_before_ms, marked_at_ms — plus the bool
        // detach_stale_parents with #[serde(default)] only (no
        // skip-empty so it always surfaces). The empty-policy wire
        // form is exactly the single key {detach_stale_parents: false};
        // dropping the skip-empty arm on any Option would silently
        // inflate every default-policy audit row with four null
        // columns and split downstream consumers that filter on field
        // presence to count tier-scoped compactions.
        let default_wire = serde_json::to_value(MemoryCompactionPolicy::default()).unwrap();
        let default_obj = default_wire
            .as_object()
            .expect("MemoryCompactionPolicy serialises as a JSON object");
        let default_keys: std::collections::BTreeSet<&str> =
            default_obj.keys().map(String::as_str).collect();
        let default_expected: std::collections::BTreeSet<&str> =
            ["detach_stale_parents"].into_iter().collect();
        assert_eq!(
            default_keys, default_expected,
            "default MemoryCompactionPolicy wire form must be exactly one \
             key (detach_stale_parents=false); the four Option<u64> cutoffs \
             are skipped via skip_serializing_if = Option::is_none",
        );
        assert_eq!(
            default_obj.get("detach_stale_parents"),
            Some(&serde_json::json!(false)),
            "detach_stale_parents must surface as false on the default \
             wire form — a skip_serializing_if = std::ops::Not::not would \
             drop the key and break consumers that filter on key presence",
        );

        let populated = MemoryCompactionPolicy {
            delete_working_before_ms: Some(1),
            delete_episodic_before_ms: Some(2),
            mark_longterm_stale_before_ms: Some(3),
            detach_stale_parents: true,
            marked_at_ms: Some(4),
        };
        let populated_wire = serde_json::to_value(&populated).unwrap();
        let populated_obj = populated_wire.as_object().unwrap();
        let populated_keys: std::collections::BTreeSet<&str> =
            populated_obj.keys().map(String::as_str).collect();
        let populated_expected: std::collections::BTreeSet<&str> = [
            "delete_working_before_ms",
            "delete_episodic_before_ms",
            "mark_longterm_stale_before_ms",
            "detach_stale_parents",
            "marked_at_ms",
        ]
        .into_iter()
        .collect();
        assert_eq!(
            populated_keys, populated_expected,
            "fully-populated MemoryCompactionPolicy wire form must be \
             exactly the five documented fields",
        );

        // Empty-object decode yields the Default (every field has
        // #[serde(default)]). A refactor that drops the default on any
        // Option<u64> would refuse a historic row that omitted the
        // cutoff and silently disable that tier's compaction on daemon
        // reopen.
        let from_empty: MemoryCompactionPolicy = serde_json::from_str("{}").unwrap();
        assert_eq!(
            from_empty,
            MemoryCompactionPolicy::default(),
            "empty JSON object must round-trip to MemoryCompactionPolicy::default()",
        );

        // Round-trip pins the PartialEq derive contract on both paths.
        let back_default: MemoryCompactionPolicy =
            serde_json::from_value(default_wire.clone()).unwrap();
        assert_eq!(back_default, MemoryCompactionPolicy::default());
        let back_populated: MemoryCompactionPolicy =
            serde_json::from_value(populated_wire).unwrap();
        assert_eq!(back_populated, populated);
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
        let obj = wire
            .as_object()
            .expect("SettlementReceipt serializes as a JSON object");
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

    #[test]
    fn settlement_receipt_strict_required_fields_reject_on_omission() {
        // SettlementReceipt is the durable JSONL row every receipt is
        // persisted as and the wire form for IPC RecentReceipts / HTTP
        // /receipts. Five strictly required fields with no serde
        // attributes — id, payer, resource, credits_consumed, settled_at —
        // anchor the contract every downstream reconciler depends on:
        // credits_consumed must not default to 0 (zero-bills the agent),
        // settled_at must not default to 0 (collapses every row to epoch
        // start and breaks recent-receipts pagination ordering).
        // settlement_receipt_memory_record_id_skip_empty_pin (line 1392)
        // pins the asymmetric Option contract; this test pins the
        // strict-required-fields rejection contract that the existing
        // tests do not exercise.
        let memory_id = Uuid::new_v4();
        let fully_populated = SettlementReceipt {
            id: Uuid::nil(),
            payer: dummy_id(),
            resource: ResourceKind::Memory,
            memory_record_id: Some(memory_id),
            credits_consumed: 42,
            settled_at: 1_700_000_000_000,
            chain: Some("solana".into()),
            cluster: Some("devnet".into()),
            batch_id: Some("batch-1".into()),
            merkle_root: Some("root".into()),
            tx_sig: Some("sig".into()),
            slot: Some(7),
            confirmed_at: Some(99),
            onchain_sig: Some("sig".into()),
        };

        let wire = serde_json::to_value(&fully_populated).unwrap();
        let obj = wire
            .as_object()
            .expect("SettlementReceipt serializes as a JSON object");
        assert_eq!(
            obj.len(),
            14,
            "fully-populated SettlementReceipt with memory_record_id=Some must surface all 14 keys on the wire; a skip_serializing_if regression on any field would silently shrink the column set and break downstream reconcilers",
        );

        let back: SettlementReceipt = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, fully_populated,
            "SettlementReceipt must round-trip through serde_json verbatim — the PartialEq + Eq derive is the contract every reconciler joins on",
        );

        for required in ["id", "payer", "resource", "credits_consumed", "settled_at"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<SettlementReceipt>(serde_json::Value::Object(missing))
                    .is_err(),
                "SettlementReceipt wire form must reject a payload missing {required:?}; a stray #[serde(default)] on a required field would silently let a malformed row decode (credits_consumed=0 zero-bills the agent, settled_at=0 collapses ordering)",
            );
        }

        let unconfirmed = SettlementReceipt {
            memory_record_id: None,
            chain: None,
            cluster: None,
            batch_id: None,
            merkle_root: None,
            tx_sig: None,
            slot: None,
            confirmed_at: None,
            onchain_sig: None,
            ..fully_populated.clone()
        };
        let wire = serde_json::to_value(&unconfirmed).unwrap();
        assert_eq!(
            wire.as_object().unwrap().len(),
            13,
            "unconfirmed SettlementReceipt with memory_record_id=None must surface exactly 13 keys (5 required + 8 null chain-metadata Options; memory_record_id is skipped); a skip_serializing_if on any of the eight chain-metadata Options would silently shrink the unconfirmed wire form below 13 and break dashboards keyed on field presence",
        );
    }

    #[test]
    fn memory_record_serde_pins_eight_field_wire_form() {
        // MemoryRecord is the persisted shape for every SQLite-backed
        // memory row and the wire form for IPC RecentMemory / HTTP /memory
        // responses. The struct holds eight fields: id, tier, owner, text,
        // embedding, metadata, created_at are strictly required, and
        // parent carries #[serde(default)] with NO #[serde(skip_serializing_if)]
        // so the wire always emits all eight keys and stale CLIs decode
        // legacy rows that predate the parent column.
        let id = Uuid::nil();
        let other = Uuid::new_v4();
        let record = MemoryRecord {
            id,
            tier: MemoryTier::Working,
            owner: dummy_id(),
            text: "note".into(),
            embedding: vec![1.0, 2.0],
            metadata: serde_json::json!({"k": "v"}),
            created_at: 1_700_000_000_000,
            parent: None,
        };

        let wire = serde_json::to_value(&record).unwrap();
        let keys: std::collections::BTreeSet<&str> = wire
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let expected: std::collections::BTreeSet<&str> = [
            "id",
            "tier",
            "owner",
            "text",
            "embedding",
            "metadata",
            "created_at",
            "parent",
        ]
        .into_iter()
        .collect();
        assert_eq!(
            keys, expected,
            "MemoryRecord wire form must be exactly eight keys; a skip_serializing_if on parent would silently drop a key for top-level records and break IPC consumers destructuring on the eight-key shape",
        );
        assert!(
            wire.get("parent").unwrap().is_null(),
            "parent: None must surface as JSON null (no skip_serializing_if on the Option<Uuid> field)",
        );

        let with_parent = MemoryRecord {
            parent: Some(other),
            ..record.clone()
        };
        let parent_wire = serde_json::to_value(&with_parent).unwrap();
        assert_eq!(
            parent_wire.get("parent").unwrap(),
            &serde_json::Value::String(other.to_string()),
            "parent: Some(uuid) must emit the uuid as a JSON string",
        );

        // Round-trip pins the PartialEq derive contract on the eight fields.
        let back: MemoryRecord = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(back, record);

        // Omitting parent must decode as None — the #[serde(default)] path
        // is what keeps stale CLI readers compatible with new daemon rows
        // and what lets legacy SQLite rows (written before the parent
        // column existed) reopen cleanly.
        let mut without_parent = wire.as_object().unwrap().clone();
        without_parent.remove("parent");
        let legacy: MemoryRecord =
            serde_json::from_value(serde_json::Value::Object(without_parent)).unwrap();
        assert_eq!(
            legacy.parent, None,
            "omitted parent must decode to None via #[serde(default)] — a dropped attribute strands every legacy memory row",
        );

        // Each strictly-required field must reject when omitted. Walk the
        // seven required keys explicitly so a refactor that flips one to
        // optional is loud at the boundary instead of through a confusing
        // upstream error.
        for required in [
            "id",
            "tier",
            "owner",
            "text",
            "embedding",
            "metadata",
            "created_at",
        ] {
            let mut missing = wire.as_object().unwrap().clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<MemoryRecord>(serde_json::Value::Object(missing)).is_err(),
                "MemoryRecord wire form must reject a payload missing {required:?}",
            );
        }

        // Cross-binding sanity: tier on a Working record must serialize to
        // the lowercase slug "working", matching the contract pinned by
        // memory_tier_serde_pins_canonical_longterm_and_legacy_aliases.
        assert_eq!(wire.get("tier").unwrap(), &serde_json::json!("working"));
    }

    #[test]
    fn memory_compaction_outcome_serde_pins_six_required_fields() {
        // MemoryCompactionOutcome is the wire shape every daemon, HTTP,
        // and CLI memory-compaction command returns, and the same envelope
        // the MemoryCompactionApplied audit row destructures. All six
        // fields are strictly required on both sides — no serde
        // attributes, every Vec<Uuid> surfaces as a JSON array even when
        // empty. A skip_serializing_if = Vec::is_empty on any of the three
        // Uuid vectors would silently drop those keys for no-op
        // compactions and split audit consumers across two shapes.
        let empty = MemoryCompactionOutcome {
            mode: MemoryRepairMode::Apply,
            would_change: true,
            changed: false,
            deleted: vec![],
            stale_marked: vec![],
            parents_detached: vec![],
        };

        let wire = serde_json::to_value(&empty).unwrap();
        let keys: std::collections::BTreeSet<&str> = wire
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let expected: std::collections::BTreeSet<&str> = [
            "mode",
            "would_change",
            "changed",
            "deleted",
            "stale_marked",
            "parents_detached",
        ]
        .into_iter()
        .collect();
        assert_eq!(
            keys, expected,
            "MemoryCompactionOutcome wire form must be exactly six keys; a skip_serializing_if = Vec::is_empty on any uuid vector would silently shift a no-op compaction's wire shape and break the MemoryCompactionApplied audit consumers",
        );

        for empty_vec_key in ["deleted", "stale_marked", "parents_detached"] {
            assert_eq!(
                wire.get(empty_vec_key).unwrap(),
                &serde_json::json!([]),
                "{empty_vec_key} must surface as an empty JSON array — pinning no skip_serializing_if on the Vec<Uuid> fields so a no-op compaction is structurally distinct from a missing field",
            );
        }

        // Populated deleted vec must round-trip with the uuid string.
        let id = Uuid::nil();
        let populated = MemoryCompactionOutcome {
            mode: MemoryRepairMode::Apply,
            would_change: true,
            changed: true,
            deleted: vec![id],
            stale_marked: vec![],
            parents_detached: vec![],
        };
        let populated_wire = serde_json::to_value(&populated).unwrap();
        assert_eq!(
            populated_wire.get("deleted").unwrap(),
            &serde_json::json!([id.to_string()]),
            "populated deleted vec must emit each uuid as a JSON string",
        );
        let back: MemoryCompactionOutcome = serde_json::from_value(populated_wire).unwrap();
        assert_eq!(back, populated);

        // Round-trip on the empty case pins the PartialEq + Eq derive contract.
        let back_empty: MemoryCompactionOutcome = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(back_empty, empty);

        // Each strictly-required field must reject when omitted.
        for required in [
            "mode",
            "would_change",
            "changed",
            "deleted",
            "stale_marked",
            "parents_detached",
        ] {
            let mut missing = wire.as_object().unwrap().clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<MemoryCompactionOutcome>(serde_json::Value::Object(
                    missing
                ))
                .is_err(),
                "MemoryCompactionOutcome wire form must reject a payload missing {required:?}",
            );
        }

        // Cross-binding: mode on an Apply outcome must serialize to the
        // snake_case slug "apply", matching memory_repair_mode_serde_pins_snake_case_wire_form.
        assert_eq!(wire.get("mode").unwrap(), &serde_json::json!("apply"));
    }

    #[test]
    fn budget_pause_checkpoint_serde_pins_strict_required_fields() {
        // BudgetPauseCheckpoint is the JSONL row JsonlPauseCheckpointStore
        // writes for every paused intent and reads back on the daemon's
        // resume-claim path. Eight fields are strictly required; the ninth
        // (resume_state) carries #[serde(default, skip_serializing_if =
        // serde_json::Map::is_empty)] — already pinned by
        // budget_pause_checkpoint_version_pins_one_and_resume_state_skips_empty.
        // This test stands alone as the strict-required pin so a refactor
        // that flips one of the eight required fields to optional fails
        // loud at the boundary instead of silently defaulting on the
        // resume-claim path.
        let intent_id = Uuid::new_v4();
        let mut resume_state = serde_json::Map::new();
        resume_state.insert("k".into(), serde_json::json!(1));
        let checkpoint = BudgetPauseCheckpoint {
            version: BudgetPauseCheckpoint::VERSION,
            intent_id,
            agent: dummy_id(),
            reason: BudgetPauseReason::BudgetExhausted,
            requested_credits: 50,
            tokens_remaining: 12,
            refill_eta_ms: 60_000,
            saved_at_ms: 1_700_000_000_000,
            resume_state,
        };

        let wire = serde_json::to_value(&checkpoint).unwrap();
        let keys: std::collections::BTreeSet<&str> = wire
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let expected: std::collections::BTreeSet<&str> = [
            "version",
            "intent_id",
            "agent",
            "reason",
            "requested_credits",
            "tokens_remaining",
            "refill_eta_ms",
            "saved_at_ms",
            "resume_state",
        ]
        .into_iter()
        .collect();
        assert_eq!(
            keys, expected,
            "BudgetPauseCheckpoint fully populated must emit exactly nine keys; a skip_serializing_if on any required field would silently shift the wire shape and break the daemon's resume-claim destructure",
        );

        // Round-trip pins the PartialEq derive on every field.
        let back: BudgetPauseCheckpoint = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(back, checkpoint);

        // Each of the eight strictly-required fields must reject when
        // omitted. resume_state is excluded from this loop because it
        // carries #[serde(default)] — its decode-as-empty arm is asserted
        // separately below.
        for required in [
            "version",
            "intent_id",
            "agent",
            "reason",
            "requested_credits",
            "tokens_remaining",
            "refill_eta_ms",
            "saved_at_ms",
        ] {
            let mut missing = wire.as_object().unwrap().clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<BudgetPauseCheckpoint>(serde_json::Value::Object(missing))
                    .is_err(),
                "BudgetPauseCheckpoint wire form must reject a payload missing {required:?}",
            );
        }

        // resume_state omitted must decode to an empty map — the
        // #[serde(default)] arm covering pre-resume_state JSONL rows.
        // Asserted here for self-containment of this strict-required pin.
        let mut without_resume = wire.as_object().unwrap().clone();
        without_resume.remove("resume_state");
        let pre_resume: BudgetPauseCheckpoint =
            serde_json::from_value(serde_json::Value::Object(without_resume)).unwrap();
        assert!(
            pre_resume.resume_state.is_empty(),
            "omitted resume_state must decode to an empty Map via #[serde(default)]",
        );

        // Cross-binding: reason on a BudgetExhausted variant must
        // serialize to the snake_case slug "budget_exhausted", matching
        // budget_pause_reason_serde_pins_snake_case_wire_form.
        assert_eq!(
            wire.get("reason").unwrap(),
            &serde_json::json!("budget_exhausted"),
        );
    }

    #[test]
    fn memory_repair_request_serde_pins_three_required_fields() {
        // MemoryRepairRequest is the request envelope every daemon, HTTP,
        // and CLI memory-repair command dispatches through. Three
        // strictly required fields with no serde attributes: mode,
        // command (the tagged enum already pinned), reason. A
        // skip_serializing_if on reason would silently produce
        // no-attribution audit rows; a flatten on command would leak the
        // inner action discriminator to the envelope shape.
        let request = MemoryRepairRequest {
            mode: MemoryRepairMode::Apply,
            command: MemoryRepairCommand::DeleteRecord { id: Uuid::nil() },
            reason: "because".into(),
        };

        let wire = serde_json::to_value(&request).unwrap();
        let obj = wire
            .as_object()
            .expect("MemoryRepairRequest serialises as a JSON object");
        let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        let expected: std::collections::BTreeSet<&str> =
            ["mode", "command", "reason"].into_iter().collect();
        assert_eq!(
            keys, expected,
            "MemoryRepairRequest wire form must be exactly three top-level \
             keys; a flatten on command would leak the inner action \
             discriminator and a skip_serializing_if on reason would drop \
             audit attribution",
        );

        let command_obj = wire
            .get("command")
            .and_then(serde_json::Value::as_object)
            .expect("command must serialise as a nested JSON object");
        assert_eq!(
            command_obj.get("action"),
            Some(&serde_json::json!("delete_record")),
            "MemoryRepairCommand discriminator must remain nested under \
             the command field, tagged \"action\" with snake_case slug \
             — cross-binding to memory_repair_command_serde_pins_each_snake_case_action_slug",
        );

        // Round-trip pins the PartialEq derive contract.
        let back: MemoryRepairRequest = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(back, request);

        // Each strictly-required field must reject when omitted.
        for required in ["mode", "command", "reason"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<MemoryRepairRequest>(serde_json::Value::Object(missing))
                    .is_err(),
                "MemoryRepairRequest wire form must reject a payload missing {required:?}",
            );
        }

        // Cross-binding: mode on an Apply request must serialise to
        // "apply" (memory_repair_mode_serde_pins_snake_case_wire_form).
        assert_eq!(wire.get("mode").unwrap(), &serde_json::json!("apply"));
    }

    #[test]
    fn memory_compaction_request_serde_pins_three_required_fields() {
        // MemoryCompactionRequest is the envelope every daemon, HTTP,
        // and CLI memory-compaction command consumes. Three strictly
        // required fields with no serde attributes: mode, policy, reason.
        // The inner MemoryCompactionPolicy itself uses
        // #[serde(default, skip_serializing_if)] on its Options, so the
        // policy wire shape varies with population; the envelope shape
        // does not.
        let request = MemoryCompactionRequest {
            mode: MemoryRepairMode::DryRun,
            policy: MemoryCompactionPolicy {
                delete_working_before_ms: Some(123),
                ..MemoryCompactionPolicy::default()
            },
            reason: "vacuum".into(),
        };

        let wire = serde_json::to_value(&request).unwrap();
        let obj = wire
            .as_object()
            .expect("MemoryCompactionRequest serialises as a JSON object");
        let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        let expected: std::collections::BTreeSet<&str> =
            ["mode", "policy", "reason"].into_iter().collect();
        assert_eq!(
            keys, expected,
            "MemoryCompactionRequest wire form must be exactly three \
             top-level keys; a flatten on policy would leak the inner \
             policy fields and a skip_serializing_if on reason would drop \
             audit attribution",
        );

        // policy must remain nested with the populated field present and
        // the non-default bool surfaced; the three None Options stay
        // dropped (already pinned by memory_compaction_policy_is_empty_pins_four_field_contract).
        let policy_obj = wire
            .get("policy")
            .and_then(serde_json::Value::as_object)
            .expect("policy must serialise as a nested JSON object");
        let policy_keys: std::collections::BTreeSet<&str> =
            policy_obj.keys().map(String::as_str).collect();
        let policy_expected: std::collections::BTreeSet<&str> =
            ["delete_working_before_ms", "detach_stale_parents"]
                .into_iter()
                .collect();
        assert_eq!(
            policy_keys, policy_expected,
            "policy nested object must surface only the populated Option \
             (delete_working_before_ms) and the always-present bool \
             (detach_stale_parents); the other three None Options must \
             stay out of the wire via skip_serializing_if",
        );

        // Round-trip pins the PartialEq derive contract.
        let back: MemoryCompactionRequest = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(back, request);

        // Each strictly-required field must reject when omitted.
        for required in ["mode", "policy", "reason"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<MemoryCompactionRequest>(serde_json::Value::Object(
                    missing,
                ))
                .is_err(),
                "MemoryCompactionRequest wire form must reject a payload missing {required:?}",
            );
        }

        // Cross-binding: mode on a DryRun request must serialise to the
        // snake_case slug "dry_run" (memory_repair_mode_serde_pins_snake_case_wire_form).
        assert_eq!(wire.get("mode").unwrap(), &serde_json::json!("dry_run"));
    }

    #[test]
    fn agent_id_error_invalid_display_display_message_pins_prefix_namespace_qualifier_and_debug_formatted_payload() {
        let err = AgentIdError::InvalidDisplay("local@bad host".into());
        let message = format!("{err}");
        assert_eq!(
            message, "invalid AgentId.display: \"local@bad host\"",
            "AgentIdError::InvalidDisplay Display drifted (typo, dropped namespace qualifier, separator swap, or Debug-vs-Display formatting regression class)"
        );
        assert!(
            message.contains("invalid AgentId.display"),
            "AgentIdError::InvalidDisplay must surface the 'AgentId.display' namespace qualifier so audit-log filters can distinguish wire-derived AgentId rejections from sibling 'display' or 'agent id' rejections (dropped-qualifier regression class): {message}"
        );
        assert!(
            message.contains("\"local@bad host\""),
            "AgentIdError::InvalidDisplay must surface the payload with surrounding double quotes (the {{0:?}} Debug-formatting), so a refactor to {{0}} that would let control bytes inject into log lines surfaces immediately (log-injection / Debug-vs-Display regression class): {message}"
        );
        assert!(
            !message.ends_with(": local@bad host"),
            "AgentIdError::InvalidDisplay must NOT end with the unquoted payload; the {{0:?}} Debug-formatting must preserve the surrounding quotes (Debug-vs-Display formatting regression class): {message}"
        );
        assert!(
            !message.contains(": local@bad host\""),
            "AgentIdError::InvalidDisplay must NOT surface a missing-leading-quote variant; both opening and closing quotes from {{0:?}} Debug-formatting must be intact (partial-quote regression class): {message}"
        );
    }
}
