//! Capability-token primitive for Covenant.
//!
//! A [`SignedCapability`] is a [`Capability`] (subject, action, scope,
//! granted-by, optional expiry) plus an ed25519 signature by the
//! granter over a deterministic byte encoding of those fields. The
//! encoder is hand-rolled and length-prefixed; it lives in one place
//! ([`canonical_message`]) and can be replaced without disturbing the
//! wire format.
//!
//! Two storage backends implement [`CapabilityStore`]:
//! [`JsonlCapabilityStore`] for production and
//! [`InMemoryCapabilityStore`] for tests. Both honour revocation
//! tombstones written via [`CapabilityStore::revoke`].

#![deny(unsafe_code)]

use async_trait::async_trait;
use covenant_types::Capability;
use ed25519_dalek::{Signature, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum PermissionError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("ed25519: {0}")]
    Crypto(#[from] ed25519_dalek::SignatureError),
    #[error("capability expired at {0}")]
    Expired(u64),
    #[error("signature does not verify against granted_by pubkey")]
    BadSignature,
    #[error("granted_by pubkey does not match the daemon trust root")]
    UntrustedGrantor,
    #[error("invalid capability scope: {0}")]
    InvalidScope(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SignedCapability {
    pub capability: Capability,
    #[serde(with = "sig_b58")]
    pub signature: [u8; 64],
}

mod sig_b58 {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&bs58::encode(v).into_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        let s = String::deserialize(d)?;
        let bytes = bs58::decode(&s)
            .into_vec()
            .map_err(serde::de::Error::custom)?;
        if bytes.len() != 64 {
            return Err(serde::de::Error::custom(format!(
                "expected 64-byte signature, got {}",
                bytes.len()
            )));
        }
        let mut arr = [0u8; 64];
        arr.copy_from_slice(&bytes);
        Ok(arr)
    }
}

#[derive(Debug, Clone, Copy)]
enum ScopeNamespace {
    Intent,
    Tool,
    Memory,
    Agent,
    A2a,
    Audit,
    Peers,
    Identity,
    Chain,
}

impl ScopeNamespace {
    fn from_action(action: &str) -> Option<Self> {
        if action.starts_with("intent.") {
            Some(Self::Intent)
        } else if action.starts_with("tool.") {
            Some(Self::Tool)
        } else if action.starts_with("memory.") {
            Some(Self::Memory)
        } else if action.starts_with("agent.") {
            Some(Self::Agent)
        } else if action.starts_with("a2a.") {
            Some(Self::A2a)
        } else if action.starts_with("audit.") {
            Some(Self::Audit)
        } else if action.starts_with("peers.") {
            Some(Self::Peers)
        } else if action.starts_with("identity.") {
            Some(Self::Identity)
        } else if action.starts_with("chain.") {
            Some(Self::Chain)
        } else {
            None
        }
    }
}

pub fn validate_scope(action: &str, scope: &Value) -> Result<(), PermissionError> {
    let Some(namespace) = ScopeNamespace::from_action(action) else {
        return Ok(());
    };
    let Some(obj) = scope.as_object() else {
        return Err(invalid_scope(action, "scope must be a JSON object"));
    };
    if obj.is_empty() {
        return Ok(());
    }
    match obj.get("version").and_then(Value::as_u64) {
        Some(1) => {}
        Some(version) => {
            return Err(invalid_scope(
                action,
                format!("unsupported scope version {version}"),
            ));
        }
        None => return Err(invalid_scope(action, "non-empty scopes must set version 1")),
    }

    match namespace {
        ScopeNamespace::Intent | ScopeNamespace::Agent => Ok(()),
        ScopeNamespace::Tool => validate_tool_scope(action, obj),
        ScopeNamespace::Memory => validate_memory_scope(action, obj),
        ScopeNamespace::A2a => validate_a2a_scope(action, obj),
        ScopeNamespace::Audit => validate_audit_scope(action, obj),
        ScopeNamespace::Peers | ScopeNamespace::Identity => validate_peer_scope(action, obj),
        ScopeNamespace::Chain => validate_chain_scope(action, obj),
    }
}

pub fn tool_call_scope_allows(
    action: &str,
    scope: &Value,
    tool_name: &str,
    arguments: &Value,
) -> Result<bool, PermissionError> {
    validate_scope(action, scope)?;
    if action != format!("tool.call.{tool_name}") {
        return Ok(false);
    }
    let Some(obj) = scope.as_object() else {
        return Ok(false);
    };
    if obj.is_empty() {
        return Ok(true);
    }
    if let Some(tool) = obj.get("tool").and_then(Value::as_str) {
        if tool != tool_name {
            return Ok(false);
        }
    }
    match obj
        .get("arguments")
        .and_then(Value::as_object)
        .and_then(|arguments| arguments.get("allow"))
    {
        Some(allow) => Ok(arguments == allow),
        None => Ok(true),
    }
}

pub fn audit_purge_scope_allows(
    action: &str,
    scope: &Value,
    before_ms: u64,
) -> Result<bool, PermissionError> {
    validate_scope(action, scope)?;
    if action != "audit.purge" {
        return Ok(false);
    }
    let Some(obj) = scope.as_object() else {
        return Ok(false);
    };
    if obj.is_empty() {
        return Ok(true);
    }
    match obj.get("before_ms") {
        Some(value) if value.is_null() => Ok(true),
        Some(value) => Ok(before_ms <= value.as_u64().unwrap_or(0)),
        None => Ok(true),
    }
}

pub fn capabilities_purge_scope_allows(
    action: &str,
    scope: &Value,
    before_ms: u64,
) -> Result<bool, PermissionError> {
    validate_scope(action, scope)?;
    if action != "capabilities.purge" {
        return Ok(false);
    }
    let Some(obj) = scope.as_object() else {
        return Ok(false);
    };
    if obj.is_empty() {
        return Ok(true);
    }
    match obj.get("before_ms") {
        Some(value) if value.is_null() => Ok(true),
        Some(value) => Ok(before_ms <= value.as_u64().unwrap_or(0)),
        None => Ok(true),
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct A2aScopeRequest<'a> {
    pub peer_pubkey_b58: Option<&'a str>,
    pub task_id: Option<&'a str>,
    pub lease_id: Option<&'a str>,
    pub duplicate_risk: Option<&'a str>,
}

pub fn a2a_scope_allows(
    action: &str,
    scope: &Value,
    expected_action: &str,
    request: A2aScopeRequest<'_>,
) -> Result<bool, PermissionError> {
    validate_scope(action, scope)?;
    if action != expected_action {
        return Ok(false);
    }
    let Some(obj) = scope.as_object() else {
        return Ok(false);
    };
    if obj.is_empty() {
        return Ok(true);
    }
    Ok(
        scope_allows_string(obj, "peer_pubkey_b58", request.peer_pubkey_b58)
            && scope_allows_string(obj, "task_id", request.task_id)
            && scope_allows_string(obj, "lease_id", request.lease_id)
            && scope_allows_duplicate_risk(obj, request.duplicate_risk),
    )
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PeerScopeRequest<'a> {
    pub peer_pubkey_b58: Option<&'a str>,
    pub token_prefix: Option<&'a str>,
    pub self_target: Option<bool>,
    pub force: Option<bool>,
    pub before_ms: Option<u64>,
}

pub fn peer_scope_allows(
    action: &str,
    scope: &Value,
    expected_action: &str,
    request: PeerScopeRequest<'_>,
) -> Result<bool, PermissionError> {
    validate_scope(action, scope)?;
    if action != expected_action {
        return Ok(false);
    }
    let Some(obj) = scope.as_object() else {
        return Ok(false);
    };
    if obj.is_empty() {
        return Ok(true);
    }
    Ok(
        scope_allows_string(obj, "peer_pubkey_b58", request.peer_pubkey_b58)
            && scope_allows_token_prefix(obj, request.token_prefix)
            && scope_allows_optional_bool(obj, "self", request.self_target)
            && scope_allows_optional_bool(obj, "force", request.force)
            && scope_allows_optional_before_ms(obj, request.before_ms),
    )
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChainScopeRequest<'a> {
    pub limit: Option<usize>,
    pub payer_pubkey_b58: Option<&'a str>,
    pub resource: Option<&'a str>,
    pub cluster: Option<&'a str>,
    pub mint: Option<&'a str>,
    pub batch_id: Option<&'a str>,
}

pub fn chain_scope_allows(
    action: &str,
    scope: &Value,
    expected_action: &str,
    request: ChainScopeRequest<'_>,
) -> Result<bool, PermissionError> {
    validate_scope(action, scope)?;
    if action != expected_action {
        return Ok(false);
    }
    let Some(obj) = scope.as_object() else {
        return Ok(false);
    };
    if obj.is_empty() {
        return Ok(true);
    }
    Ok(scope_allows_optional_limit(obj, request.limit)
        && scope_allows_optional_string(obj, "payer_pubkey_b58", request.payer_pubkey_b58)
        && scope_allows_optional_string(obj, "resource", request.resource)
        && scope_allows_optional_string(obj, "cluster", request.cluster)
        && scope_allows_optional_string(obj, "mint", request.mint)
        && scope_allows_optional_string(obj, "batch_id", request.batch_id))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryCompactionScopeRequest {
    pub apply: bool,
    pub delete_working_before_ms: Option<u64>,
    pub delete_episodic_before_ms: Option<u64>,
    pub mark_longterm_stale_before_ms: Option<u64>,
    pub detach_stale_parents: bool,
}

pub fn memory_purge_scope_allows(
    action: &str,
    scope: &Value,
    tier: Option<&str>,
    before_ms: u64,
) -> Result<bool, PermissionError> {
    validate_scope(action, scope)?;
    if action != "memory.purge" {
        return Ok(false);
    }
    let Some(obj) = scope.as_object() else {
        return Ok(false);
    };
    if obj.is_empty() {
        return Ok(true);
    }
    if !scope_allows_apply(obj, true) || !scope_allows_before_ms(obj, before_ms) {
        return Ok(false);
    }
    match tier {
        Some(tier) => Ok(scope_allows_tiers(obj, &[tier])),
        None => Ok(scope_allows_tiers(
            obj,
            &["working", "episodic", "longterm"],
        )),
    }
}

pub fn memory_read_scope_allows(
    action: &str,
    scope: &Value,
    tier: Option<&str>,
) -> Result<bool, PermissionError> {
    validate_scope(action, scope)?;
    if !memory_read_action_allows(action, tier) {
        return Ok(false);
    }
    let Some(obj) = scope.as_object() else {
        return Ok(false);
    };
    if obj.is_empty() {
        return Ok(true);
    }
    if !scope_allows_apply(obj, false) {
        return Ok(false);
    }
    match tier {
        Some(tier) => Ok(scope_allows_tiers(obj, &[tier])),
        None => Ok(true),
    }
}

pub fn memory_read_record_scope_allows(
    action: &str,
    scope: &Value,
    record_id: &str,
    tier: &str,
    created_at_ms: u64,
) -> Result<bool, PermissionError> {
    validate_scope(action, scope)?;
    if !memory_read_action_allows(action, Some(tier)) {
        return Ok(false);
    }
    let Some(obj) = scope.as_object() else {
        return Ok(false);
    };
    if obj.is_empty() {
        return Ok(true);
    }
    if !scope_allows_apply(obj, false)
        || !scope_allows_record_id(obj, record_id)
        || !scope_allows_tiers(obj, &[tier])
    {
        return Ok(false);
    }
    match obj.get("before_ms") {
        Some(value) if value.is_null() => Ok(true),
        Some(value) => Ok(created_at_ms < value.as_u64().unwrap_or(0)),
        None => Ok(true),
    }
}

pub fn memory_write_scope_allows(
    action: &str,
    scope: &Value,
    record_id: &str,
    tier: &str,
    created_at_ms: u64,
) -> Result<bool, PermissionError> {
    validate_scope(action, scope)?;
    if action != "memory.write" {
        return Ok(false);
    }
    let Some(obj) = scope.as_object() else {
        return Ok(false);
    };
    if obj.is_empty() {
        return Ok(true);
    }
    if !scope_allows_apply(obj, true)
        || !scope_allows_record_id(obj, record_id)
        || !scope_allows_tiers(obj, &[tier])
    {
        return Ok(false);
    }
    match obj.get("before_ms") {
        Some(value) if value.is_null() => Ok(true),
        Some(value) => Ok(created_at_ms < value.as_u64().unwrap_or(0)),
        None => Ok(true),
    }
}

pub fn memory_repair_scope_allows(
    action: &str,
    scope: &Value,
    record_id: &str,
    tier: &str,
    created_at_ms: u64,
    apply: bool,
) -> Result<bool, PermissionError> {
    validate_scope(action, scope)?;
    let expected_action = if apply {
        "memory.repair.apply"
    } else {
        "memory.repair.dry_run"
    };
    if action != expected_action {
        return Ok(false);
    }
    let Some(obj) = scope.as_object() else {
        return Ok(false);
    };
    if obj.is_empty() {
        return Ok(true);
    }
    if !scope_allows_apply(obj, apply)
        || !scope_allows_record_id(obj, record_id)
        || !scope_allows_tiers(obj, &[tier])
    {
        return Ok(false);
    }
    match obj.get("before_ms") {
        Some(value) if value.is_null() => Ok(true),
        Some(value) => Ok(created_at_ms < value.as_u64().unwrap_or(0)),
        None => Ok(true),
    }
}

pub fn memory_compaction_scope_allows(
    action: &str,
    scope: &Value,
    request: MemoryCompactionScopeRequest,
) -> Result<bool, PermissionError> {
    validate_scope(action, scope)?;
    let expected_action = if request.apply {
        "memory.compact.apply"
    } else {
        "memory.compact.dry_run"
    };
    if action != expected_action {
        return Ok(false);
    }
    let Some(obj) = scope.as_object() else {
        return Ok(false);
    };
    if obj.is_empty() {
        return Ok(true);
    }
    if !scope_allows_apply(obj, request.apply) {
        return Ok(false);
    }
    for before_ms in [
        request.delete_working_before_ms,
        request.delete_episodic_before_ms,
        request.mark_longterm_stale_before_ms,
    ]
    .into_iter()
    .flatten()
    {
        if !scope_allows_before_ms(obj, before_ms) {
            return Ok(false);
        }
    }
    if request.delete_working_before_ms.is_some() && !scope_allows_tiers(obj, &["working"]) {
        return Ok(false);
    }
    if request.delete_episodic_before_ms.is_some() && !scope_allows_tiers(obj, &["episodic"]) {
        return Ok(false);
    }
    if request.mark_longterm_stale_before_ms.is_some() && !scope_allows_tiers(obj, &["longterm"]) {
        return Ok(false);
    }
    if request.detach_stale_parents
        && obj.contains_key("tiers")
        && !scope_allows_tiers(obj, &["working", "episodic", "longterm"])
    {
        return Ok(false);
    }
    Ok(true)
}

fn memory_read_action_allows(action: &str, tier: Option<&str>) -> bool {
    if action == "memory.read" {
        return true;
    }
    let Some(action_tier) = action.strip_prefix("memory.read.") else {
        return false;
    };
    tier.is_some_and(|tier| tier == action_tier)
}

fn scope_allows_apply(obj: &Map<String, Value>, apply: bool) -> bool {
    obj.get("apply")
        .and_then(Value::as_bool)
        .map(|allowed| allowed == apply)
        .unwrap_or(true)
}

fn scope_allows_record_id(obj: &Map<String, Value>, record_id: &str) -> bool {
    obj.get("record_id")
        .and_then(Value::as_str)
        .map(|allowed| allowed == record_id)
        .unwrap_or(true)
}

fn scope_allows_before_ms(obj: &Map<String, Value>, before_ms: u64) -> bool {
    match obj.get("before_ms") {
        Some(value) if value.is_null() => true,
        Some(value) => before_ms <= value.as_u64().unwrap_or(0),
        None => true,
    }
}

fn scope_allows_string(obj: &Map<String, Value>, field: &str, actual: Option<&str>) -> bool {
    match obj.get(field) {
        Some(value) if value.is_null() => true,
        Some(value) => value.as_str() == actual,
        None => true,
    }
}

fn scope_allows_token_prefix(obj: &Map<String, Value>, actual: Option<&str>) -> bool {
    match obj.get("token_prefix") {
        Some(value) if value.is_null() => true,
        Some(value) => match (value.as_str(), actual) {
            (Some(expected), Some(actual)) => actual.starts_with(expected),
            _ => false,
        },
        None => true,
    }
}

fn scope_allows_optional_bool(obj: &Map<String, Value>, field: &str, actual: Option<bool>) -> bool {
    match obj.get(field) {
        Some(value) if value.is_null() => true,
        Some(value) => actual
            .map(|actual| value.as_bool() == Some(actual))
            .unwrap_or(false),
        None => true,
    }
}

fn scope_allows_optional_before_ms(obj: &Map<String, Value>, before_ms: Option<u64>) -> bool {
    match obj.get("before_ms") {
        Some(value) if value.is_null() => true,
        Some(value) => before_ms
            .map(|before_ms| before_ms <= value.as_u64().unwrap_or(0))
            .unwrap_or(false),
        None => true,
    }
}

fn scope_allows_optional_limit(obj: &Map<String, Value>, actual: Option<usize>) -> bool {
    match obj.get("limit") {
        Some(value) => actual
            .map(|actual| (actual as u64) <= value.as_u64().unwrap_or(0))
            .unwrap_or(true),
        None => true,
    }
}

fn scope_allows_optional_string(
    obj: &Map<String, Value>,
    field: &str,
    actual: Option<&str>,
) -> bool {
    match obj.get(field) {
        Some(value) if value.is_null() => true,
        Some(value) => actual
            .map(|actual| value.as_str() == Some(actual))
            .unwrap_or(true),
        None => true,
    }
}

fn scope_allows_duplicate_risk(obj: &Map<String, Value>, actual: Option<&str>) -> bool {
    match obj.get("duplicate_risk") {
        Some(value) if value.is_null() => true,
        Some(value) => {
            let Some(actual) = actual else {
                return false;
            };
            value
                .as_str()
                .map(|expected| expected.replace('_', "-") == actual.replace('_', "-"))
                .unwrap_or(false)
        }
        None => true,
    }
}

fn scope_allows_tiers(obj: &Map<String, Value>, requested: &[&str]) -> bool {
    let Some(tiers) = obj.get("tiers").and_then(Value::as_array) else {
        return true;
    };
    requested.iter().all(|requested| {
        tiers
            .iter()
            .filter_map(Value::as_str)
            .any(|allowed| allowed == *requested)
    })
}

fn validate_tool_scope(action: &str, obj: &Map<String, Value>) -> Result<(), PermissionError> {
    optional_string_or_null(action, obj, "tool")?;
    if let (Some(expected), Some(tool)) = (
        action.strip_prefix("tool.call."),
        obj.get("tool").and_then(Value::as_str),
    ) {
        if tool != expected {
            return Err(invalid_scope(
                action,
                format!("tool must match action suffix {expected:?}"),
            ));
        }
    }
    if let Some(arguments) = obj.get("arguments") {
        let Some(arguments) = arguments.as_object() else {
            return Err(invalid_scope(action, "arguments must be an object"));
        };
        if let Some(allow) = arguments.get("allow") {
            if !allow.is_object() {
                return Err(invalid_scope(action, "arguments.allow must be an object"));
            }
        }
    }
    Ok(())
}

fn validate_memory_scope(action: &str, obj: &Map<String, Value>) -> Result<(), PermissionError> {
    optional_string_array(action, obj, "tiers", &["working", "episodic", "longterm"])?;
    optional_string_or_null(action, obj, "record_id")?;
    optional_non_negative_integer_or_null(action, obj, "before_ms")?;
    optional_bool(action, obj, "apply")?;
    Ok(())
}

fn validate_a2a_scope(action: &str, obj: &Map<String, Value>) -> Result<(), PermissionError> {
    optional_string_or_null(action, obj, "peer_pubkey_b58")?;
    optional_string_or_null(action, obj, "task_id")?;
    optional_string_or_null(action, obj, "lease_id")?;
    optional_string_enum(
        action,
        obj,
        "duplicate_risk",
        &["idempotent", "operator-accepted", "operator_accepted"],
    )?;
    Ok(())
}

fn validate_audit_scope(action: &str, obj: &Map<String, Value>) -> Result<(), PermissionError> {
    optional_positive_integer(action, obj, "window")?;
    optional_non_negative_integer_or_null(action, obj, "before_ms")?;
    optional_bool(action, obj, "include_integrity")?;
    Ok(())
}

fn validate_peer_scope(action: &str, obj: &Map<String, Value>) -> Result<(), PermissionError> {
    optional_pubkey_b58_or_null(action, obj, "peer_pubkey_b58")?;
    optional_base58_prefix_or_null(action, obj, "token_prefix")?;
    optional_bool_or_null(action, obj, "self")?;
    optional_bool_or_null(action, obj, "force")?;
    optional_non_negative_integer_or_null(action, obj, "before_ms")?;
    Ok(())
}

fn validate_chain_scope(action: &str, obj: &Map<String, Value>) -> Result<(), PermissionError> {
    optional_positive_integer(action, obj, "limit")?;
    optional_non_empty_string_or_null(action, obj, "mint")?;
    optional_non_empty_string_or_null(action, obj, "cluster")?;
    optional_pubkey_b58_or_null(action, obj, "payer_pubkey_b58")?;
    optional_string_enum_or_null(
        action,
        obj,
        "resource",
        &["compute", "memory", "tool", "message", "registration"],
    )?;
    optional_non_empty_string_or_null(action, obj, "batch_id")?;
    Ok(())
}

fn optional_string_or_null(
    action: &str,
    obj: &Map<String, Value>,
    field: &str,
) -> Result<(), PermissionError> {
    if let Some(value) = obj.get(field) {
        if !value.is_null() && !value.is_string() {
            return Err(invalid_scope(
                action,
                format!("{field} must be a string or null"),
            ));
        }
    }
    Ok(())
}

fn optional_pubkey_b58_or_null(
    action: &str,
    obj: &Map<String, Value>,
    field: &str,
) -> Result<(), PermissionError> {
    let Some(value) = obj.get(field) else {
        return Ok(());
    };
    if value.is_null() {
        return Ok(());
    }
    let Some(value) = value.as_str() else {
        return Err(invalid_scope(
            action,
            format!("{field} must be a base58 public key or null"),
        ));
    };
    let Ok(decoded) = bs58::decode(value).into_vec() else {
        return Err(invalid_scope(
            action,
            format!("{field} must be a base58 public key or null"),
        ));
    };
    if decoded.len() != 32 {
        return Err(invalid_scope(
            action,
            format!("{field} must decode to a 32-byte public key"),
        ));
    }
    Ok(())
}

fn optional_non_empty_string_or_null(
    action: &str,
    obj: &Map<String, Value>,
    field: &str,
) -> Result<(), PermissionError> {
    let Some(value) = obj.get(field) else {
        return Ok(());
    };
    if value.is_null() {
        return Ok(());
    }
    let Some(value) = value.as_str() else {
        return Err(invalid_scope(
            action,
            format!("{field} must be a non-empty string or null"),
        ));
    };
    if value.is_empty() {
        return Err(invalid_scope(
            action,
            format!("{field} must be a non-empty string or null"),
        ));
    }
    Ok(())
}

fn optional_base58_prefix_or_null(
    action: &str,
    obj: &Map<String, Value>,
    field: &str,
) -> Result<(), PermissionError> {
    let Some(value) = obj.get(field) else {
        return Ok(());
    };
    if value.is_null() {
        return Ok(());
    }
    let Some(value) = value.as_str() else {
        return Err(invalid_scope(
            action,
            format!("{field} must be a non-empty base58 prefix or null"),
        ));
    };
    if value.is_empty() || bs58::decode(value).into_vec().is_err() {
        return Err(invalid_scope(
            action,
            format!("{field} must be a non-empty base58 prefix or null"),
        ));
    }
    Ok(())
}

fn optional_bool(
    action: &str,
    obj: &Map<String, Value>,
    field: &str,
) -> Result<(), PermissionError> {
    if let Some(value) = obj.get(field) {
        if !value.is_boolean() {
            return Err(invalid_scope(action, format!("{field} must be a boolean")));
        }
    }
    Ok(())
}

fn optional_bool_or_null(
    action: &str,
    obj: &Map<String, Value>,
    field: &str,
) -> Result<(), PermissionError> {
    if let Some(value) = obj.get(field) {
        if !value.is_null() && !value.is_boolean() {
            return Err(invalid_scope(
                action,
                format!("{field} must be a boolean or null"),
            ));
        }
    }
    Ok(())
}

fn optional_non_negative_integer_or_null(
    action: &str,
    obj: &Map<String, Value>,
    field: &str,
) -> Result<(), PermissionError> {
    if let Some(value) = obj.get(field) {
        if !value.is_null() && value.as_u64().is_none() {
            return Err(invalid_scope(
                action,
                format!("{field} must be a non-negative integer or null"),
            ));
        }
    }
    Ok(())
}

fn optional_positive_integer(
    action: &str,
    obj: &Map<String, Value>,
    field: &str,
) -> Result<(), PermissionError> {
    if let Some(value) = obj.get(field) {
        match value.as_u64() {
            Some(value) if value > 0 => {}
            _ => {
                return Err(invalid_scope(
                    action,
                    format!("{field} must be a positive integer"),
                ));
            }
        }
    }
    Ok(())
}

fn optional_string_array(
    action: &str,
    obj: &Map<String, Value>,
    field: &str,
    allowed: &[&str],
) -> Result<(), PermissionError> {
    let Some(value) = obj.get(field) else {
        return Ok(());
    };
    let Some(values) = value.as_array() else {
        return Err(invalid_scope(action, format!("{field} must be an array")));
    };
    for value in values {
        let Some(value) = value.as_str() else {
            return Err(invalid_scope(
                action,
                format!("{field} entries must be strings"),
            ));
        };
        if !allowed.contains(&value) {
            return Err(invalid_scope(
                action,
                format!("{field} contains unsupported value {value:?}"),
            ));
        }
    }
    Ok(())
}

fn optional_string_enum(
    action: &str,
    obj: &Map<String, Value>,
    field: &str,
    allowed: &[&str],
) -> Result<(), PermissionError> {
    let Some(value) = obj.get(field) else {
        return Ok(());
    };
    let Some(value) = value.as_str() else {
        return Err(invalid_scope(action, format!("{field} must be a string")));
    };
    if !allowed.contains(&value) {
        return Err(invalid_scope(
            action,
            format!("{field} contains unsupported value {value:?}"),
        ));
    }
    Ok(())
}

fn optional_string_enum_or_null(
    action: &str,
    obj: &Map<String, Value>,
    field: &str,
    allowed: &[&str],
) -> Result<(), PermissionError> {
    let Some(value) = obj.get(field) else {
        return Ok(());
    };
    if value.is_null() {
        return Ok(());
    }
    let Some(value) = value.as_str() else {
        return Err(invalid_scope(
            action,
            format!("{field} must be a string or null"),
        ));
    };
    if !allowed.contains(&value) {
        return Err(invalid_scope(
            action,
            format!("{field} contains unsupported value {value:?}"),
        ));
    }
    Ok(())
}

fn invalid_scope(action: &str, detail: impl Into<String>) -> PermissionError {
    PermissionError::InvalidScope(format!("{action}: {}", detail.into()))
}

/// Deterministic byte encoding of a capability — what the signer signs.
///
/// Layout:
/// `subject_pubkey[32] || action_len_be[4] || action || scope_len_be[4] ||
///  scope_jcs_bytes || granted_by_pubkey[32] || expires_tag[1] || expires_at_be[8]`
///
/// `scope_jcs_bytes` is RFC 8785 (JSON Canonicalization Scheme) — keys are
/// sorted lexicographically, whitespace is removed, numbers are normalised.
/// Two scopes that are JSON-equal under any input ordering produce identical
/// signed messages, so a re-serialised cap (through any compliant parser)
/// still verifies under the same `granted_by` key.
pub fn canonical_message(cap: &Capability) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&cap.subject.pubkey);

    let action_bytes = cap.action.as_bytes();
    out.extend_from_slice(&(action_bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(action_bytes);

    let scope_bytes = serde_jcs::to_vec(&cap.scope).expect("scope serialise");
    out.extend_from_slice(&(scope_bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(&scope_bytes);

    out.extend_from_slice(&cap.granted_by.pubkey);
    out.push(if cap.expires_at.is_some() { 1 } else { 0 });
    out.extend_from_slice(&cap.expires_at.unwrap_or(0).to_be_bytes());
    out
}

/// Sign a capability with `granted_by`'s key. The caller must ensure
/// `cap.granted_by.pubkey == verifying_key(of_signing_key).to_bytes()`; this
/// fn does not enforce it (asymmetric authority delegations are valid in
/// principle).
pub fn sign(cap: Capability, signing_key: &SigningKey) -> SignedCapability {
    let msg = canonical_message(&cap);
    let signature = ed25519_dalek::Signer::sign(signing_key, &msg);
    SignedCapability {
        capability: cap,
        signature: signature.to_bytes(),
    }
}

/// Verify the signature against `cap.granted_by.pubkey`. Does **not** check
/// expiry; use `verify_with_clock` for that.
pub fn verify(signed: &SignedCapability) -> Result<(), PermissionError> {
    let vk = VerifyingKey::from_bytes(&signed.capability.granted_by.pubkey)?;
    let sig = Signature::from_bytes(&signed.signature);
    let msg = canonical_message(&signed.capability);
    vk.verify(&msg, &sig)
        .map_err(|_| PermissionError::BadSignature)
}

/// Like `verify` but also rejects an expired capability. `now_ms` is epoch
/// milliseconds; pass the daemon's clock at the point of the check.
pub fn verify_with_clock(signed: &SignedCapability, now_ms: u64) -> Result<(), PermissionError> {
    verify(signed)?;
    if let Some(exp) = signed.capability.expires_at {
        if now_ms > exp {
            return Err(PermissionError::Expired(exp));
        }
    }
    Ok(())
}

/// Verify expiry, signature, and that `granted_by.pubkey` matches the
/// configured trust root. The trust root is the daemon identity that
/// owns the capability store; rejecting any other grantor closes the
/// out-of-band-write threat where an attacker with file access to
/// `granted.jsonl` self-signs a capability with their own pubkey.
pub fn verify_with_clock_and_trust_root(
    signed: &SignedCapability,
    now_ms: u64,
    trust_root: [u8; 32],
) -> Result<(), PermissionError> {
    if signed.capability.granted_by.pubkey != trust_root {
        return Err(PermissionError::UntrustedGrantor);
    }
    verify_with_clock(signed, now_ms)
}

#[async_trait]
pub trait CapabilityStore: Send + Sync {
    async fn record(&self, signed: SignedCapability) -> Result<(), PermissionError>;
    /// Returns `true` if a matching live capability was present (and is now
    /// revoked); `false` if no live capability had that signature.
    async fn revoke(&self, signature: [u8; 64]) -> Result<bool, PermissionError>;
    async fn is_revoked(&self, signature: [u8; 64]) -> Result<bool, PermissionError>;
    async fn list_for_subject(
        &self,
        subject_pubkey: [u8; 32],
    ) -> Result<Vec<SignedCapability>, PermissionError>;
    async fn recent(&self, limit: usize) -> Result<Vec<SignedCapability>, PermissionError>;
    /// Drop every revocation with `revoked_at < before_ms` along with its
    /// matching grant. Returns the count of revocations dropped (which equals
    /// the count of grant lines also dropped, modulo any pre-existing
    /// revocation-without-grant entries — those are also dropped from
    /// `revoked.jsonl`). Operator-driven retention; the live-set remains
    /// `granted ⊝ revoked` so a grant whose revocation has been purged is
    /// **not** resurrected. Live (non-revoked) grants are never touched.
    async fn purge_revoked_older_than(&self, before_ms: u64) -> Result<u64, PermissionError>;
}

/// Revocation record. The daemon writes one of these per `revoke()` call;
/// a capability is treated as live iff its signature is in `granted.jsonl`
/// and **not** in `revoked.jsonl`. Revocations are themselves not signed
/// (the daemon's local identity is the trust root for v0); a future
/// `Phase 5+` could add countersignatures.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Revocation {
    #[serde(with = "sig_b58")]
    pub signature: [u8; 64],
    pub revoked_at: u64,
}

pub struct JsonlCapabilityStore {
    granted_path: PathBuf,
    revoked_path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl JsonlCapabilityStore {
    /// `granted_path` should typically be `$COVENANT_HOME/capabilities/granted.jsonl`;
    /// the matching revocation log lives next to it as `revoked.jsonl`.
    pub async fn open(granted_path: PathBuf) -> Result<Self, PermissionError> {
        if let Some(parent) = granted_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&granted_path)
            .await?;
        let revoked_path = granted_path
            .parent()
            .map(|p| p.join("revoked.jsonl"))
            .unwrap_or_else(|| PathBuf::from("revoked.jsonl"));
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&revoked_path)
            .await?;
        Ok(Self {
            granted_path,
            revoked_path,
            lock: Arc::new(Mutex::new(())),
        })
    }

    async fn read_all_grants(&self) -> Result<Vec<SignedCapability>, PermissionError> {
        Self::read_jsonl(&self.granted_path).await
    }

    async fn read_all_revocations(&self) -> Result<Vec<Revocation>, PermissionError> {
        Self::read_jsonl(&self.revoked_path).await
    }

    async fn read_jsonl<T: serde::de::DeserializeOwned>(
        path: &std::path::Path,
    ) -> Result<Vec<T>, PermissionError> {
        let f = match fs::File::open(path).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut reader = BufReader::new(f);
        let mut all = Vec::new();
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break;
            }
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                continue;
            }
            all.push(serde_json::from_str(trimmed)?);
        }
        Ok(all)
    }

    fn epoch_ms() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

#[async_trait]
impl CapabilityStore for JsonlCapabilityStore {
    async fn record(&self, signed: SignedCapability) -> Result<(), PermissionError> {
        let _g = self.lock.lock().await;
        let line = serde_json::to_string(&signed)?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.granted_path)
            .await?;
        f.write_all(line.as_bytes()).await?;
        f.write_all(b"\n").await?;
        f.flush().await?;
        Ok(())
    }

    async fn revoke(&self, signature: [u8; 64]) -> Result<bool, PermissionError> {
        let _g = self.lock.lock().await;
        let already_revoked = Self::read_jsonl::<Revocation>(&self.revoked_path)
            .await?
            .iter()
            .any(|r| r.signature == signature);
        if already_revoked {
            return Ok(false);
        }
        let was_granted = Self::read_jsonl::<SignedCapability>(&self.granted_path)
            .await?
            .iter()
            .any(|c| c.signature == signature);
        if !was_granted {
            return Ok(false);
        }
        let rev = Revocation {
            signature,
            revoked_at: Self::epoch_ms(),
        };
        let line = serde_json::to_string(&rev)?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.revoked_path)
            .await?;
        f.write_all(line.as_bytes()).await?;
        f.write_all(b"\n").await?;
        f.flush().await?;
        Ok(true)
    }

    async fn is_revoked(&self, signature: [u8; 64]) -> Result<bool, PermissionError> {
        let _g = self.lock.lock().await;
        Ok(Self::read_jsonl::<Revocation>(&self.revoked_path)
            .await?
            .iter()
            .any(|r| r.signature == signature))
    }

    async fn list_for_subject(
        &self,
        subject_pubkey: [u8; 32],
    ) -> Result<Vec<SignedCapability>, PermissionError> {
        let _g = self.lock.lock().await;
        let revoked: std::collections::HashSet<[u8; 64]> = self
            .read_all_revocations()
            .await?
            .into_iter()
            .map(|r| r.signature)
            .collect();
        Ok(self
            .read_all_grants()
            .await?
            .into_iter()
            .filter(|s| s.capability.subject.pubkey == subject_pubkey)
            .filter(|s| !revoked.contains(&s.signature))
            .collect())
    }

    async fn recent(&self, limit: usize) -> Result<Vec<SignedCapability>, PermissionError> {
        let _g = self.lock.lock().await;
        let revoked: std::collections::HashSet<[u8; 64]> = self
            .read_all_revocations()
            .await?
            .into_iter()
            .map(|r| r.signature)
            .collect();
        let mut live: Vec<SignedCapability> = self
            .read_all_grants()
            .await?
            .into_iter()
            .filter(|s| !revoked.contains(&s.signature))
            .collect();
        let start = live.len().saturating_sub(limit);
        Ok(live.split_off(start))
    }

    async fn purge_revoked_older_than(&self, before_ms: u64) -> Result<u64, PermissionError> {
        // Read-filter-rewrite under the same lock that record / revoke use, so
        // a concurrent grant/revoke can't race with the rewrite. Atomicity of
        // the per-file rewrite comes from `tempfile + rename`. The two files
        // are rewritten sequentially: the granted file first (so a crash mid
        // rewrite leaves a strict superset of revoked entries — the live-set
        // is unchanged), then the revoked file.
        let _g = self.lock.lock().await;

        let revocations = Self::read_jsonl::<Revocation>(&self.revoked_path).await?;
        let drop_sigs: std::collections::HashSet<[u8; 64]> = revocations
            .iter()
            .filter(|r| r.revoked_at < before_ms)
            .map(|r| r.signature)
            .collect();
        let purged = drop_sigs.len() as u64;
        if purged == 0 {
            return Ok(0);
        }

        let grants = Self::read_jsonl::<SignedCapability>(&self.granted_path).await?;
        let kept_grants: Vec<&SignedCapability> = grants
            .iter()
            .filter(|s| !drop_sigs.contains(&s.signature))
            .collect();
        let kept_revocations: Vec<&Revocation> = revocations
            .iter()
            .filter(|r| !drop_sigs.contains(&r.signature))
            .collect();

        Self::rewrite_atomically(&self.granted_path, &kept_grants).await?;
        Self::rewrite_atomically(&self.revoked_path, &kept_revocations).await?;
        Ok(purged)
    }
}

impl JsonlCapabilityStore {
    async fn rewrite_atomically<T: serde::Serialize>(
        path: &std::path::Path,
        rows: &[&T],
    ) -> Result<(), PermissionError> {
        let tmp_path = path.with_extension("jsonl.tmp");
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)
            .await?;
        for r in rows {
            let line = serde_json::to_string(r)?;
            f.write_all(line.as_bytes()).await?;
            f.write_all(b"\n").await?;
        }
        f.flush().await?;
        drop(f);
        fs::rename(&tmp_path, path).await?;
        Ok(())
    }
}

#[derive(Default)]
pub struct InMemoryCapabilityStore {
    granted: Mutex<Vec<SignedCapability>>,
    revoked: Mutex<std::collections::HashMap<[u8; 64], u64>>,
}

impl InMemoryCapabilityStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl CapabilityStore for InMemoryCapabilityStore {
    async fn record(&self, signed: SignedCapability) -> Result<(), PermissionError> {
        self.granted.lock().await.push(signed);
        Ok(())
    }

    async fn revoke(&self, signature: [u8; 64]) -> Result<bool, PermissionError> {
        let mut revoked = self.revoked.lock().await;
        if revoked.contains_key(&signature) {
            return Ok(false);
        }
        let granted = self.granted.lock().await;
        if !granted.iter().any(|c| c.signature == signature) {
            return Ok(false);
        }
        revoked.insert(signature, JsonlCapabilityStore::epoch_ms());
        Ok(true)
    }

    async fn is_revoked(&self, signature: [u8; 64]) -> Result<bool, PermissionError> {
        Ok(self.revoked.lock().await.contains_key(&signature))
    }

    async fn list_for_subject(
        &self,
        subject_pubkey: [u8; 32],
    ) -> Result<Vec<SignedCapability>, PermissionError> {
        let revoked = self.revoked.lock().await;
        let granted = self.granted.lock().await;
        Ok(granted
            .iter()
            .filter(|s| s.capability.subject.pubkey == subject_pubkey)
            .filter(|s| !revoked.contains_key(&s.signature))
            .cloned()
            .collect())
    }

    async fn recent(&self, limit: usize) -> Result<Vec<SignedCapability>, PermissionError> {
        let revoked = self.revoked.lock().await;
        let granted = self.granted.lock().await;
        let live: Vec<SignedCapability> = granted
            .iter()
            .filter(|s| !revoked.contains_key(&s.signature))
            .cloned()
            .collect();
        let start = live.len().saturating_sub(limit);
        Ok(live[start..].to_vec())
    }

    async fn purge_revoked_older_than(&self, before_ms: u64) -> Result<u64, PermissionError> {
        let mut revoked = self.revoked.lock().await;
        let drop_sigs: Vec<[u8; 64]> = revoked
            .iter()
            .filter(|(_, ts)| **ts < before_ms)
            .map(|(s, _)| *s)
            .collect();
        let purged = drop_sigs.len() as u64;
        if purged == 0 {
            return Ok(0);
        }
        let drop_set: std::collections::HashSet<[u8; 64]> = drop_sigs.iter().copied().collect();
        for s in &drop_sigs {
            revoked.remove(s);
        }
        let mut granted = self.granted.lock().await;
        granted.retain(|c| !drop_set.contains(&c.signature));
        Ok(purged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_identity::LocalIdentity;
    use covenant_types::AgentId;

    fn cap(
        subject: AgentId,
        action: &str,
        granted_by: AgentId,
        expires_at: Option<u64>,
    ) -> Capability {
        Capability {
            subject,
            action: action.into(),
            scope: serde_json::json!({ "path": "research/*" }),
            granted_by,
            expires_at,
        }
    }

    fn assert_invalid_scope(action: &str, scope: serde_json::Value) {
        assert!(matches!(
            validate_scope(action, &scope),
            Err(PermissionError::InvalidScope(_))
        ));
    }

    #[test]
    fn validate_scope_accepts_empty_and_unknown_scopes() {
        assert!(validate_scope("tool.web_search", &serde_json::json!({})).is_ok());
        assert!(validate_scope("custom.action", &serde_json::json!("opaque")).is_ok());
    }

    #[test]
    fn validate_scope_accepts_known_versioned_shapes() {
        let cases = [
            (
                "intent.delegate",
                serde_json::json!({
                    "version": 1,
                    "priority": "normal"
                }),
            ),
            (
                "tool.call.echo",
                serde_json::json!({
                    "version": 1,
                    "tool": "echo",
                    "arguments": { "allow": { "text": "hello" } }
                }),
            ),
            (
                "memory.write",
                serde_json::json!({
                    "version": 1,
                    "tiers": ["working", "episodic"],
                    "record_id": null,
                    "before_ms": null,
                    "apply": false
                }),
            ),
            (
                "agent.spawn",
                serde_json::json!({
                    "version": 1,
                    "agent_id": "research"
                }),
            ),
            (
                "a2a.requeue",
                serde_json::json!({
                    "version": 1,
                    "peer_pubkey_b58": "peer",
                    "task_id": null,
                    "lease_id": null,
                    "duplicate_risk": "idempotent"
                }),
            ),
            (
                "audit.verify",
                serde_json::json!({
                    "version": 1,
                    "window": 100,
                    "before_ms": null,
                    "include_integrity": true
                }),
            ),
            (
                "peers.revoke",
                serde_json::json!({
                    "version": 1,
                    "peer_pubkey_b58": null,
                    "token_prefix": "abc123",
                    "self": null,
                    "force": true,
                    "before_ms": null
                }),
            ),
            (
                "chain.flush",
                serde_json::json!({
                    "version": 1,
                    "limit": 10,
                    "mint": null,
                    "cluster": "localnet",
                    "payer_pubkey_b58": null,
                    "resource": "memory",
                    "batch_id": null
                }),
            ),
        ];

        for (action, scope) in cases {
            assert!(validate_scope(action, &scope).is_ok(), "{action}");
        }
    }

    #[test]
    fn validate_scope_rejects_known_non_object_scopes() {
        assert_invalid_scope("tool.web_search", serde_json::json!("bad"));
        assert_invalid_scope("memory.write", serde_json::json!(["bad"]));
    }

    #[test]
    fn validate_scope_rejects_missing_or_unsupported_version() {
        assert_invalid_scope("memory.write", serde_json::json!({ "tiers": ["working"] }));
        assert_invalid_scope("memory.write", serde_json::json!({ "version": 2 }));
        assert_invalid_scope("memory.write", serde_json::json!({ "version": "1" }));
    }

    #[test]
    fn validate_scope_rejects_invalid_known_fields() {
        assert_invalid_scope(
            "tool.call.echo",
            serde_json::json!({ "version": 1, "tool": "search" }),
        );
        assert_invalid_scope(
            "tool.call.echo",
            serde_json::json!({ "version": 1, "arguments": { "allow": ["text"] } }),
        );
        assert_invalid_scope(
            "memory.write",
            serde_json::json!({ "version": 1, "tiers": ["archive"] }),
        );
        assert_invalid_scope(
            "a2a.requeue",
            serde_json::json!({ "version": 1, "duplicate_risk": "unknown" }),
        );
        assert_invalid_scope(
            "audit.verify",
            serde_json::json!({ "version": 1, "window": 0 }),
        );
        assert_invalid_scope(
            "peers.revoke",
            serde_json::json!({ "version": 1, "force": "yes" }),
        );
        assert_invalid_scope(
            "peers.revoke",
            serde_json::json!({ "version": 1, "token_prefix": "" }),
        );
        assert_invalid_scope(
            "peers.revoke",
            serde_json::json!({ "version": 1, "peer_pubkey_b58": "peer-1" }),
        );
        assert_invalid_scope(
            "chain.flush",
            serde_json::json!({ "version": 1, "limit": -1 }),
        );
        assert_invalid_scope(
            "chain.flush",
            serde_json::json!({ "version": 1, "payer_pubkey_b58": "peer-1" }),
        );
        assert_invalid_scope(
            "chain.flush",
            serde_json::json!({ "version": 1, "resource": "unknown" }),
        );
        assert_invalid_scope(
            "chain.flush",
            serde_json::json!({ "version": 1, "batch_id": "" }),
        );
    }

    #[test]
    fn tool_call_scope_allows_unscoped_grants() {
        assert!(tool_call_scope_allows(
            "tool.call.echo",
            &serde_json::json!({}),
            "echo",
            &serde_json::json!({ "text": "hi" })
        )
        .unwrap());
    }

    #[test]
    fn tool_call_scope_allows_exact_allowed_arguments() {
        let scope = serde_json::json!({
            "version": 1,
            "tool": "echo",
            "arguments": { "allow": { "text": "hi" } }
        });
        assert!(tool_call_scope_allows(
            "tool.call.echo",
            &scope,
            "echo",
            &serde_json::json!({ "text": "hi" })
        )
        .unwrap());
    }

    #[test]
    fn tool_call_scope_rejects_argument_mismatch() {
        let scope = serde_json::json!({
            "version": 1,
            "tool": "echo",
            "arguments": { "allow": { "text": "hi" } }
        });
        assert!(!tool_call_scope_allows(
            "tool.call.echo",
            &scope,
            "echo",
            &serde_json::json!({ "text": "bye" })
        )
        .unwrap());
        assert!(!tool_call_scope_allows(
            "tool.call.echo",
            &scope,
            "echo",
            &serde_json::json!({ "text": "hi", "extra": true })
        )
        .unwrap());
    }

    #[test]
    fn tool_call_scope_rejects_action_or_scope_mismatch() {
        assert!(!tool_call_scope_allows(
            "tool.call.search",
            &serde_json::json!({}),
            "echo",
            &serde_json::json!({})
        )
        .unwrap());
        assert!(matches!(
            tool_call_scope_allows(
                "tool.call.echo",
                &serde_json::json!({ "version": 2 }),
                "echo",
                &serde_json::json!({})
            ),
            Err(PermissionError::InvalidScope(_))
        ));
    }

    #[test]
    fn audit_purge_scope_allows_unscoped_grants() {
        assert!(audit_purge_scope_allows("audit.purge", &serde_json::json!({}), 1_000).unwrap());
    }

    #[test]
    fn audit_purge_scope_allows_cutoff_within_scope() {
        let scope = serde_json::json!({
            "version": 1,
            "before_ms": 1_000
        });
        assert!(audit_purge_scope_allows("audit.purge", &scope, 999).unwrap());
        assert!(audit_purge_scope_allows("audit.purge", &scope, 1_000).unwrap());
    }

    #[test]
    fn audit_purge_scope_rejects_cutoff_beyond_scope() {
        let scope = serde_json::json!({
            "version": 1,
            "before_ms": 1_000
        });
        assert!(!audit_purge_scope_allows("audit.purge", &scope, 1_001).unwrap());
        assert!(!audit_purge_scope_allows("audit.verify", &serde_json::json!({}), 1_000).unwrap());
    }

    #[test]
    fn capabilities_purge_scope_allows_unscoped_grants() {
        assert!(capabilities_purge_scope_allows(
            "capabilities.purge",
            &serde_json::json!({}),
            1_000
        )
        .unwrap());
    }

    #[test]
    fn capabilities_purge_scope_allows_cutoff_within_scope() {
        let scope = serde_json::json!({
            "version": 1,
            "before_ms": 1_000
        });
        assert!(capabilities_purge_scope_allows("capabilities.purge", &scope, 999).unwrap());
        assert!(capabilities_purge_scope_allows("capabilities.purge", &scope, 1_000).unwrap());
    }

    #[test]
    fn capabilities_purge_scope_rejects_cutoff_beyond_scope() {
        let scope = serde_json::json!({
            "version": 1,
            "before_ms": 1_000
        });
        assert!(!capabilities_purge_scope_allows("capabilities.purge", &scope, 1_001).unwrap());
        assert!(
            !capabilities_purge_scope_allows("audit.purge", &serde_json::json!({}), 1_000).unwrap()
        );
    }

    #[test]
    fn a2a_scope_allows_peer_task_lease_and_duplicate_risk() {
        let scope = serde_json::json!({
            "version": 1,
            "peer_pubkey_b58": "peer-1",
            "task_id": "task-1",
            "lease_id": "lease-1",
            "duplicate_risk": "idempotent"
        });
        let request = A2aScopeRequest {
            peer_pubkey_b58: Some("peer-1"),
            task_id: Some("task-1"),
            lease_id: Some("lease-1"),
            duplicate_risk: Some("idempotent"),
        };
        assert!(
            a2a_scope_allows("a2a.repair.requeue", &scope, "a2a.repair.requeue", request).unwrap()
        );

        let wrong_peer = A2aScopeRequest {
            peer_pubkey_b58: Some("peer-2"),
            ..request
        };
        assert!(!a2a_scope_allows(
            "a2a.repair.requeue",
            &scope,
            "a2a.repair.requeue",
            wrong_peer
        )
        .unwrap());

        let wrong_task = A2aScopeRequest {
            task_id: Some("task-2"),
            ..request
        };
        assert!(!a2a_scope_allows(
            "a2a.repair.requeue",
            &scope,
            "a2a.repair.requeue",
            wrong_task
        )
        .unwrap());

        let wrong_lease = A2aScopeRequest {
            lease_id: Some("lease-2"),
            ..request
        };
        assert!(!a2a_scope_allows(
            "a2a.repair.requeue",
            &scope,
            "a2a.repair.requeue",
            wrong_lease
        )
        .unwrap());

        let wrong_risk = A2aScopeRequest {
            duplicate_risk: Some("operator-accepted"),
            ..request
        };
        assert!(!a2a_scope_allows(
            "a2a.repair.requeue",
            &scope,
            "a2a.repair.requeue",
            wrong_risk
        )
        .unwrap());
    }

    #[test]
    fn a2a_scope_allows_unscoped_and_rejects_action_mismatch() {
        let request = A2aScopeRequest {
            peer_pubkey_b58: Some("peer-1"),
            task_id: Some("task-1"),
            lease_id: None,
            duplicate_risk: None,
        };
        assert!(a2a_scope_allows(
            "a2a.send.peer",
            &serde_json::json!({}),
            "a2a.send.peer",
            request
        )
        .unwrap());
        assert!(!a2a_scope_allows(
            "a2a.send.peer",
            &serde_json::json!({}),
            "a2a.send.other",
            request
        )
        .unwrap());
    }

    #[test]
    fn peer_scope_allows_peer_token_force_and_cutoff_predicates() {
        let peer_pubkey_b58 = bs58::encode([1u8; 32]).into_string();
        let scope = serde_json::json!({
            "version": 1,
            "peer_pubkey_b58": peer_pubkey_b58,
            "token_prefix": "abc123",
            "self": false,
            "force": false,
            "before_ms": 1_000
        });
        let request = PeerScopeRequest {
            peer_pubkey_b58: Some(&peer_pubkey_b58),
            token_prefix: Some("abc123fff"),
            self_target: Some(false),
            force: Some(false),
            before_ms: Some(999),
        };
        assert!(peer_scope_allows("peers.revoke", &scope, "peers.revoke", request).unwrap());

        assert!(!peer_scope_allows(
            "peers.revoke",
            &scope,
            "peers.revoke",
            PeerScopeRequest {
                token_prefix: Some("ab"),
                ..request
            }
        )
        .unwrap());
        assert!(!peer_scope_allows(
            "peers.revoke",
            &scope,
            "peers.revoke",
            PeerScopeRequest {
                force: Some(true),
                ..request
            }
        )
        .unwrap());
        assert!(!peer_scope_allows(
            "peers.purge",
            &scope,
            "peers.purge",
            PeerScopeRequest {
                before_ms: Some(1_001),
                ..request
            }
        )
        .unwrap());
    }

    #[test]
    fn peer_scope_allows_unscoped_and_rejects_action_mismatch() {
        let request = PeerScopeRequest {
            peer_pubkey_b58: Some("4vJ9JU1bJJE96FWSKczs4eeH9YnCMMpuSBjtpy6nG6GU"),
            token_prefix: None,
            self_target: None,
            force: None,
            before_ms: None,
        };
        assert!(
            peer_scope_allows("peers.list", &serde_json::json!({}), "peers.list", request).unwrap()
        );
        assert!(!peer_scope_allows(
            "peers.list",
            &serde_json::json!({}),
            "peers.revoke",
            request
        )
        .unwrap());
    }

    #[test]
    fn chain_scope_allows_limit_environment_and_receipt_predicates() {
        let payer = bs58::encode([9u8; 32]).into_string();
        let scope = serde_json::json!({
            "version": 1,
            "limit": 10,
            "payer_pubkey_b58": payer,
            "resource": "memory",
            "cluster": "devnet",
            "mint": "So11111111111111111111111111111111111111112",
            "batch_id": "batch-1"
        });
        let request = ChainScopeRequest {
            limit: Some(5),
            payer_pubkey_b58: Some(&payer),
            resource: Some("memory"),
            cluster: Some("devnet"),
            mint: Some("So11111111111111111111111111111111111111112"),
            batch_id: Some("batch-1"),
        };
        assert!(chain_scope_allows("chain.flush", &scope, "chain.flush", request).unwrap());
        assert!(!chain_scope_allows(
            "chain.flush",
            &scope,
            "chain.flush",
            ChainScopeRequest {
                limit: Some(11),
                ..request
            }
        )
        .unwrap());
        assert!(!chain_scope_allows(
            "chain.flush",
            &scope,
            "chain.flush",
            ChainScopeRequest {
                resource: Some("tool"),
                ..request
            }
        )
        .unwrap());
    }

    #[test]
    fn memory_purge_scope_allows_tier_and_cutoff() {
        let scope = serde_json::json!({
            "version": 1,
            "tiers": ["working"],
            "before_ms": 1_000,
            "apply": true
        });
        assert!(memory_purge_scope_allows("memory.purge", &scope, Some("working"), 999).unwrap());
        assert!(!memory_purge_scope_allows("memory.purge", &scope, Some("episodic"), 999).unwrap());
        assert!(
            !memory_purge_scope_allows("memory.purge", &scope, Some("working"), 1_001).unwrap()
        );
        assert!(!memory_purge_scope_allows("memory.purge", &scope, None, 999).unwrap());
    }

    #[test]
    fn memory_read_scope_allows_tier_mode_and_record_filters() {
        let scope = serde_json::json!({
            "version": 1,
            "tiers": ["working"],
            "record_id": "record-1",
            "before_ms": 1_000,
            "apply": false
        });
        assert!(memory_read_scope_allows("memory.read", &scope, Some("working")).unwrap());
        assert!(!memory_read_scope_allows("memory.read", &scope, Some("episodic")).unwrap());
        assert!(memory_read_scope_allows("memory.read.working", &scope, Some("working")).unwrap());
        assert!(
            !memory_read_scope_allows("memory.read.episodic", &scope, Some("working")).unwrap()
        );
        assert!(
            memory_read_record_scope_allows("memory.read", &scope, "record-1", "working", 999)
                .unwrap()
        );
        assert!(!memory_read_record_scope_allows(
            "memory.read",
            &scope,
            "record-2",
            "working",
            999
        )
        .unwrap());
        assert!(!memory_read_record_scope_allows(
            "memory.read",
            &scope,
            "record-1",
            "working",
            1_000
        )
        .unwrap());
    }

    #[test]
    fn memory_write_scope_allows_tier_record_mode_and_cutoff() {
        let scope = serde_json::json!({
            "version": 1,
            "tiers": ["working"],
            "record_id": "record-1",
            "before_ms": 1_000,
            "apply": true
        });
        assert!(
            memory_write_scope_allows("memory.write", &scope, "record-1", "working", 999).unwrap()
        );
        assert!(
            !memory_write_scope_allows("memory.write", &scope, "record-2", "working", 999).unwrap()
        );
        assert!(
            !memory_write_scope_allows("memory.write", &scope, "record-1", "episodic", 999)
                .unwrap()
        );
        assert!(
            !memory_write_scope_allows("memory.write", &scope, "record-1", "working", 1_000)
                .unwrap()
        );

        let dry_run_scope = serde_json::json!({
            "version": 1,
            "tiers": ["working"],
            "apply": false
        });
        assert!(!memory_write_scope_allows(
            "memory.write",
            &dry_run_scope,
            "record-1",
            "working",
            999
        )
        .unwrap());
    }

    #[test]
    fn memory_repair_scope_allows_record_tier_and_mode() {
        let scope = serde_json::json!({
            "version": 1,
            "record_id": "record-1",
            "tiers": ["working"],
            "before_ms": 1_000,
            "apply": true
        });
        assert!(memory_repair_scope_allows(
            "memory.repair.apply",
            &scope,
            "record-1",
            "working",
            999,
            true
        )
        .unwrap());
        assert!(!memory_repair_scope_allows(
            "memory.repair.apply",
            &scope,
            "record-2",
            "working",
            999,
            true
        )
        .unwrap());
        assert!(!memory_repair_scope_allows(
            "memory.repair.dry_run",
            &scope,
            "record-1",
            "working",
            999,
            false
        )
        .unwrap());
        assert!(!memory_repair_scope_allows(
            "memory.repair.apply",
            &scope,
            "record-1",
            "working",
            1_000,
            true
        )
        .unwrap());
    }

    #[test]
    fn memory_compaction_scope_allows_tiers_and_cutoffs() {
        let scope = serde_json::json!({
            "version": 1,
            "tiers": ["working", "episodic"],
            "before_ms": 1_000,
            "apply": true
        });
        assert!(memory_compaction_scope_allows(
            "memory.compact.apply",
            &scope,
            MemoryCompactionScopeRequest {
                apply: true,
                delete_working_before_ms: Some(999),
                delete_episodic_before_ms: Some(1_000),
                mark_longterm_stale_before_ms: None,
                detach_stale_parents: false,
            }
        )
        .unwrap());
        assert!(!memory_compaction_scope_allows(
            "memory.compact.apply",
            &scope,
            MemoryCompactionScopeRequest {
                apply: true,
                delete_working_before_ms: None,
                delete_episodic_before_ms: None,
                mark_longterm_stale_before_ms: Some(999),
                detach_stale_parents: false,
            }
        )
        .unwrap());
        assert!(!memory_compaction_scope_allows(
            "memory.compact.apply",
            &scope,
            MemoryCompactionScopeRequest {
                apply: true,
                delete_working_before_ms: Some(1_001),
                delete_episodic_before_ms: None,
                mark_longterm_stale_before_ms: None,
                detach_stale_parents: false,
            }
        )
        .unwrap());
        assert!(!memory_compaction_scope_allows(
            "memory.compact.apply",
            &scope,
            MemoryCompactionScopeRequest {
                apply: true,
                delete_working_before_ms: None,
                delete_episodic_before_ms: None,
                mark_longterm_stale_before_ms: None,
                detach_stale_parents: true,
            }
        )
        .unwrap());
    }

    #[test]
    fn sign_and_verify_round_trip() {
        let issuer = LocalIdentity::generate("authority@local");
        let subject = LocalIdentity::generate("research@local").agent_id();
        let signed = sign(
            cap(subject, "tool.web_search", issuer.agent_id(), None),
            issuer.signing_key(),
        );
        assert!(verify(&signed).is_ok());
    }

    #[test]
    fn canonical_message_is_jcs_stable_across_scope_key_orderings() {
        // RFC 8785 (JCS) sorts keys lexicographically. A cap signed with one
        // scope ordering must verify after the scope has been round-tripped
        // through any compliant parser, even one that re-orders keys.
        let issuer = LocalIdentity::generate("authority@local");
        let subject = LocalIdentity::generate("research@local").agent_id();
        let mut cap_a = cap(subject.clone(), "tool.web_search", issuer.agent_id(), None);
        cap_a.scope = serde_json::json!({
            "alpha": 1,
            "beta": 2,
            "gamma": { "nested_b": false, "nested_a": true },
        });
        let signed = sign(cap_a, issuer.signing_key());

        let mut reordered = signed.clone();
        reordered.capability.scope = serde_json::json!({
            "gamma": { "nested_a": true, "nested_b": false },
            "beta": 2,
            "alpha": 1,
        });
        assert!(
            verify(&reordered).is_ok(),
            "JCS canonicalisation must accept key-reordered scope"
        );

        let mut tampered = signed.clone();
        tampered.capability.scope = serde_json::json!({
            "alpha": 1,
            "beta": 999,
            "gamma": { "nested_b": false, "nested_a": true },
        });
        assert!(
            matches!(verify(&tampered), Err(PermissionError::BadSignature)),
            "JCS canonicalisation must reject value tampering"
        );
    }

    #[test]
    fn verify_rejects_tampered_action() {
        let issuer = LocalIdentity::generate("authority@local");
        let subject = LocalIdentity::generate("research@local").agent_id();
        let mut signed = sign(
            cap(subject, "tool.web_search", issuer.agent_id(), None),
            issuer.signing_key(),
        );
        signed.capability.action = "tool.gpu_inference".into();
        assert!(matches!(
            verify(&signed),
            Err(PermissionError::BadSignature)
        ));
    }

    #[test]
    fn verify_with_clock_rejects_expired() {
        let issuer = LocalIdentity::generate("authority@local");
        let subject = LocalIdentity::generate("research@local").agent_id();
        let signed = sign(
            cap(subject, "tool.web_search", issuer.agent_id(), Some(1000)),
            issuer.signing_key(),
        );
        assert!(verify_with_clock(&signed, 999).is_ok());
        assert!(matches!(
            verify_with_clock(&signed, 1001),
            Err(PermissionError::Expired(1000))
        ));
    }

    #[tokio::test]
    async fn in_memory_store_filters_by_subject() {
        let issuer = LocalIdentity::generate("authority@local");
        let alice = LocalIdentity::generate("alice@local").agent_id();
        let bob = LocalIdentity::generate("bob@local").agent_id();

        let s = InMemoryCapabilityStore::new();
        s.record(sign(
            cap(alice.clone(), "tool.web_search", issuer.agent_id(), None),
            issuer.signing_key(),
        ))
        .await
        .unwrap();
        s.record(sign(
            cap(bob.clone(), "memory.write", issuer.agent_id(), None),
            issuer.signing_key(),
        ))
        .await
        .unwrap();

        let alice_caps = s.list_for_subject(alice.pubkey).await.unwrap();
        assert_eq!(alice_caps.len(), 1);
        assert_eq!(alice_caps[0].capability.action, "tool.web_search");
    }

    #[tokio::test]
    async fn jsonl_round_trip_through_a_real_file() {
        let issuer = LocalIdentity::generate("authority@local");
        let subject = LocalIdentity::generate("research@local").agent_id();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("granted.jsonl");
        let s = JsonlCapabilityStore::open(path.clone()).await.unwrap();
        s.record(sign(
            cap(subject, "tool.web_search", issuer.agent_id(), None),
            issuer.signing_key(),
        ))
        .await
        .unwrap();

        let s2 = JsonlCapabilityStore::open(path.clone()).await.unwrap();
        let recent = s2.recent(10).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert!(verify(&recent[0]).is_ok());
    }

    #[test]
    fn signed_capability_round_trips_through_serde() {
        let issuer = LocalIdentity::generate("authority@local");
        let subject = LocalIdentity::generate("research@local").agent_id();
        let signed = sign(
            cap(subject, "memory.write", issuer.agent_id(), Some(123_456)),
            issuer.signing_key(),
        );
        let json = serde_json::to_string(&signed).unwrap();
        let back: SignedCapability = serde_json::from_str(&json).unwrap();
        assert_eq!(signed, back);
        assert!(verify(&back).is_ok());
    }

    #[tokio::test]
    async fn in_memory_revoke_removes_from_subject_list() {
        let issuer = LocalIdentity::generate("authority@local");
        let subject = LocalIdentity::generate("research@local").agent_id();
        let signed = sign(
            cap(subject.clone(), "tool.web_search", issuer.agent_id(), None),
            issuer.signing_key(),
        );
        let s = InMemoryCapabilityStore::new();
        s.record(signed.clone()).await.unwrap();

        assert_eq!(s.list_for_subject(subject.pubkey).await.unwrap().len(), 1);
        assert!(s.revoke(signed.signature).await.unwrap());
        assert!(s.is_revoked(signed.signature).await.unwrap());
        assert_eq!(s.list_for_subject(subject.pubkey).await.unwrap().len(), 0);
        // Re-revoking is a no-op.
        assert!(!s.revoke(signed.signature).await.unwrap());
    }

    #[tokio::test]
    async fn jsonl_revoke_persists_across_reopen() {
        let issuer = LocalIdentity::generate("authority@local");
        let subject = LocalIdentity::generate("research@local").agent_id();
        let signed = sign(
            cap(subject, "memory.write", issuer.agent_id(), None),
            issuer.signing_key(),
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capabilities").join("granted.jsonl");
        let s = JsonlCapabilityStore::open(path.clone()).await.unwrap();
        s.record(signed.clone()).await.unwrap();
        assert!(s.revoke(signed.signature).await.unwrap());

        let s2 = JsonlCapabilityStore::open(path).await.unwrap();
        assert!(s2.is_revoked(signed.signature).await.unwrap());
        assert!(s2.recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn revoke_unknown_signature_is_a_no_op() {
        let s = InMemoryCapabilityStore::new();
        let r = s.revoke([0u8; 64]).await.unwrap();
        assert!(!r);
    }

    #[tokio::test]
    async fn in_memory_purge_drops_old_revocations_and_their_grants() {
        let issuer = LocalIdentity::generate("authority@local");
        let subject = LocalIdentity::generate("research@local").agent_id();

        let s = InMemoryCapabilityStore::new();
        // Two grants. Only one will get revoked.
        let live = sign(
            cap(subject.clone(), "tool.web_search", issuer.agent_id(), None),
            issuer.signing_key(),
        );
        let dead = sign(
            cap(subject.clone(), "memory.write", issuer.agent_id(), None),
            issuer.signing_key(),
        );
        s.record(live.clone()).await.unwrap();
        s.record(dead.clone()).await.unwrap();
        assert!(s.revoke(dead.signature).await.unwrap());

        // Force the revocation timestamp into the past so the purge picks
        // it up. (`revoke` stamps with epoch_ms which is far in the future
        // relative to a `before_ms` of 100.)
        s.revoked.lock().await.insert(dead.signature, 50);

        let purged = s.purge_revoked_older_than(100).await.unwrap();
        assert_eq!(purged, 1);

        // Live grant survived.
        let recent = s.recent(10).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].signature, live.signature);

        // Revoked-grant entry is gone from the granted vec.
        assert_eq!(s.granted.lock().await.len(), 1);

        // The tombstone is gone too — but the live-set is unchanged because
        // the matching grant was removed in lockstep.
        assert!(!s.is_revoked(dead.signature).await.unwrap());
        assert!(s
            .list_for_subject(subject.pubkey)
            .await
            .unwrap()
            .iter()
            .all(|c| c.signature != dead.signature));
    }

    #[tokio::test]
    async fn jsonl_purge_rewrites_both_files_and_keeps_live_grants() {
        let issuer = LocalIdentity::generate("authority@local");
        let subject = LocalIdentity::generate("research@local").agent_id();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("granted.jsonl");
        let s = JsonlCapabilityStore::open(path.clone()).await.unwrap();

        let live = sign(
            cap(subject.clone(), "tool.web_search", issuer.agent_id(), None),
            issuer.signing_key(),
        );
        let dead = sign(
            cap(subject.clone(), "memory.write", issuer.agent_id(), None),
            issuer.signing_key(),
        );
        s.record(live.clone()).await.unwrap();
        s.record(dead.clone()).await.unwrap();
        assert!(s.revoke(dead.signature).await.unwrap());

        // The on-disk revocation was just stamped with `epoch_ms()`; rewrite
        // it with a deterministic past timestamp so the purge picks it up.
        let rev_path = dir.path().join("revoked.jsonl");
        let rewritten = serde_json::to_string(&Revocation {
            signature: dead.signature,
            revoked_at: 50,
        })
        .unwrap();
        std::fs::write(&rev_path, format!("{rewritten}\n")).unwrap();

        let purged = s.purge_revoked_older_than(100).await.unwrap();
        assert_eq!(purged, 1);

        // Reopen to confirm both files round-trip through serde.
        let s2 = JsonlCapabilityStore::open(path.clone()).await.unwrap();
        let recent = s2.recent(10).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].signature, live.signature);
        assert!(!s2.is_revoked(dead.signature).await.unwrap());

        // The tempfile.tmp left behind would be a leak. The atomic-rename
        // path drops it; nothing should match.
        assert!(!path.with_extension("jsonl.tmp").exists());
        assert!(!rev_path.with_extension("jsonl.tmp").exists());
    }

    #[tokio::test]
    async fn jsonl_purge_no_op_when_no_revocations_match() {
        let issuer = LocalIdentity::generate("authority@local");
        let subject = LocalIdentity::generate("research@local").agent_id();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("granted.jsonl");
        let s = JsonlCapabilityStore::open(path.clone()).await.unwrap();

        let live = sign(
            cap(subject, "tool.web_search", issuer.agent_id(), None),
            issuer.signing_key(),
        );
        s.record(live.clone()).await.unwrap();
        assert!(s.revoke(live.signature).await.unwrap());

        // Fresh revocation timestamp — `before_ms` of 100 finds nothing.
        let purged = s.purge_revoked_older_than(100).await.unwrap();
        assert_eq!(purged, 0);

        // No tempfile.tmp left behind: the atomic-rewrite branch never ran.
        assert!(!path.with_extension("jsonl.tmp").exists());
        let rev_path = dir.path().join("revoked.jsonl");
        assert!(!rev_path.with_extension("jsonl.tmp").exists());

        // Tombstone is still on disk.
        assert!(s.is_revoked(live.signature).await.unwrap());
    }

    #[tokio::test]
    async fn purge_does_not_resurrect_purged_grants() {
        // Defence-in-depth: after a purge, the dropped-grant signature must
        // not reappear in `recent`/`list_for_subject`. The grant line is gone
        // and the revocation tombstone is gone, but the live-set semantics
        // (`granted ⊝ revoked`) still report no live entry for that sig.
        let issuer = LocalIdentity::generate("authority@local");
        let subject = LocalIdentity::generate("research@local").agent_id();

        let s = InMemoryCapabilityStore::new();
        let dead = sign(
            cap(subject.clone(), "memory.write", issuer.agent_id(), None),
            issuer.signing_key(),
        );
        s.record(dead.clone()).await.unwrap();
        assert!(s.revoke(dead.signature).await.unwrap());
        s.revoked.lock().await.insert(dead.signature, 50);

        assert_eq!(s.purge_revoked_older_than(100).await.unwrap(), 1);

        // Even though `is_revoked` now reports false (tombstone gone), the
        // grant line is also gone, so neither lookup surfaces it.
        assert!(!s.is_revoked(dead.signature).await.unwrap());
        assert!(s.recent(10).await.unwrap().is_empty());
        assert!(s.list_for_subject(subject.pubkey).await.unwrap().is_empty());
    }

    #[test]
    fn scope_allows_apply_pins_absent_key_bool_match_and_non_bool_fallthrough() {
        let empty = serde_json::json!({});
        let empty = empty.as_object().unwrap();
        assert!(
            scope_allows_apply(empty, true),
            "absent 'apply' field must default to allow for apply=true; otherwise unscoped grants silently reject destructive apply operations",
        );
        assert!(
            scope_allows_apply(empty, false),
            "absent 'apply' field must default to allow for apply=false; defaulting to deny would silently reject every dry-run dispatch through an unscoped grant",
        );

        let apply_true = serde_json::json!({ "apply": true });
        let apply_true = apply_true.as_object().unwrap();
        assert!(
            scope_allows_apply(apply_true, true),
            "scope {{\"apply\": true}} must allow apply=true; otherwise the explicit allow scope contradicts the requested apply",
        );
        assert!(
            !scope_allows_apply(apply_true, false),
            "scope {{\"apply\": true}} must NOT allow apply=false; otherwise the equality check silently degrades to a one-way OR and apply=true grants authorize dry-runs they were not asked about",
        );

        let apply_false = serde_json::json!({ "apply": false });
        let apply_false = apply_false.as_object().unwrap();
        assert!(
            scope_allows_apply(apply_false, false),
            "scope {{\"apply\": false}} must allow apply=false so dry-run-only grants work",
        );
        assert!(
            !scope_allows_apply(apply_false, true),
            "scope {{\"apply\": false}} must NOT allow apply=true; a regression here would silently let a dry-run grant authorize a destructive apply",
        );

        let non_bool = serde_json::json!({ "apply": "yes" });
        let non_bool = non_bool.as_object().unwrap();
        assert!(
            scope_allows_apply(non_bool, true),
            "a non-bool 'apply' field must fall through to allow for apply=true so partially-typed scope objects match the rest of the scope_allows_* family",
        );
        assert!(
            scope_allows_apply(non_bool, false),
            "a non-bool 'apply' field must fall through to allow for apply=false; flipping this to deny would diverge from scope_allows_record_id and break operator-supplied non-strict scope objects",
        );
    }

    #[test]
    fn memory_read_action_allows_pins_umbrella_tier_match_and_missing_or_unknown_tier_rejection() {
        assert!(
            memory_read_action_allows("memory.read", None),
            "the umbrella 'memory.read' action must pass when no tier is supplied; otherwise unscoped grants cannot authorize tier-less reads",
        );
        assert!(
            memory_read_action_allows("memory.read", Some("short")),
            "the umbrella 'memory.read' action must pass regardless of supplied tier; otherwise an unrestricted grant silently stops authorizing tier-bearing reads",
        );

        assert!(
            memory_read_action_allows("memory.read.short", Some("short")),
            "the tiered 'memory.read.<tier>' action must pass on exact tier match; otherwise tier-scoped grants never authorize their own tier",
        );
        assert!(
            !memory_read_action_allows("memory.read.short", Some("long")),
            "the tiered action must NOT pass on tier mismatch; otherwise a 'short'-scoped grant silently authorizes 'long' reads",
        );
        assert!(
            !memory_read_action_allows("memory.read.short", None),
            "the tiered action must NOT pass when no tier is supplied; defaulting to allow would silently authorize tier-scoped reads with no tier context",
        );

        assert!(
            !memory_read_action_allows("memory.write", Some("short")),
            "an action that is not 'memory.read' and does not start with 'memory.read.' must be rejected; otherwise unrelated actions silently flow through the read gate",
        );
        assert!(
            !memory_read_action_allows("memory", None),
            "a partial-prefix action like 'memory' must be rejected; otherwise a non-action string silently authorizes reads",
        );
        assert!(
            !memory_read_action_allows("memory.readx", Some("short")),
            "an action with a near-prefix like 'memory.readx' must be rejected; otherwise strip_prefix matching silently widens to typo-ed prefixes",
        );
    }
}
