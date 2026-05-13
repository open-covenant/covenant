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

/// Plain-English title for a signed capability action. Mirrors the catalog
/// the operator console (covenant-web/lib/labels.ts) uses — keep them in
/// sync. Callers that need a fallback for unknown actions can use
/// [`friendly_action_title`], which returns `None` for actions outside the
/// catalog instead of inventing a title.
pub fn friendly_action_title(action: &str) -> Option<&'static str> {
    match action {
        "intent.subscribe" => Some("receive your tasks"),
        "intent.publish" => Some("send tasks to other agents"),
        "memory.read" => Some("read memory"),
        "memory.write" => Some("save to memory"),
        "memory.purge" => Some("delete memories"),
        "memory.search" => Some("search memory"),
        "identity.read" => Some("see identity info"),
        "identity.rotate" => Some("rotate identity keys"),
        "tool.web_search" => Some("search the web"),
        "tool.summarize" => Some("summarize text"),
        "tool.terminal" => Some("run terminal commands"),
        "tool.file_read" => Some("read files"),
        "tool.file_write" => Some("write files"),
        "tool.gpu_inference" => Some("use GPU inference"),
        "agent.spawn" => Some("start other agents"),
        "agent.suspend" => Some("pause other agents"),
        "chain.receipts" => Some("read settlement receipts"),
        "chain.flush" => Some("flush receipts on-chain"),
        "audit.purge" => Some("purge audit log entries"),
        "capabilities.purge" => Some("purge revoked permissions"),
        "peers.purge" => Some("purge revoked peers"),
        "a2a.compact" => Some("compact the agent-to-agent log"),
        _ => None,
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
    fn scope_namespace_from_action_pins_each_prefix_and_unknown_fallthrough() {
        // covenant_permissions::ScopeNamespace::from_action (line 88-111)
        // is the prefix-to-namespace dispatch table that routes every
        // capability scope check to the correct per-namespace
        // validator (validate_tool_scope, validate_memory_scope,
        // validate_a2a_scope, validate_audit_scope, validate_peer_scope,
        // validate_chain_scope). Nine documented prefixes plus an
        // unknown-action fallthrough that returns None so validate_scope
        // can pass through opaque actions without per-namespace
        // validation.
        //
        // The function is exercised only INDIRECTLY through
        // validate_scope tests that pair each action with its full
        // grant payload. A refactor that swapped two prefix arms
        // during a code-style cleanup (e.g., consolidating the
        // if-else ladder into a match table with arms in different
        // order) would silently route memory.write grants into the
        // audit-scope validator; the existing integration tests
        // would still pass because they always pair each action with
        // a payload that the NATIVE validator accepts, and a
        // misrouted dispatch only surfaces when the audit-scope
        // validator's field whitelist happens to differ from
        // memory-scope's in a way that the test fixture doesn't
        // touch.
        //
        // Pin each prefix arm and the fallthrough at the dispatch
        // function's boundary so a prefix-swap or arm-drop regression
        // fails loud here.
        assert!(
            matches!(
                ScopeNamespace::from_action("intent.dispatch"),
                Some(ScopeNamespace::Intent)
            ),
            "intent.* prefix must route to ScopeNamespace::Intent — a swap with another arm would route intent grants to the wrong validator",
        );
        assert!(
            matches!(
                ScopeNamespace::from_action("tool.call.echo"),
                Some(ScopeNamespace::Tool)
            ),
            "tool.* prefix must route to ScopeNamespace::Tool — a swap with the audit arm would land tool.call grants in validate_audit_scope, whose argument-allowlist contract differs",
        );
        assert!(
            matches!(
                ScopeNamespace::from_action("memory.write"),
                Some(ScopeNamespace::Memory)
            ),
            "memory.* prefix must route to ScopeNamespace::Memory — a swap with the audit arm would silently let memory.write grants pass validate_audit_scope's looser before_ms/include_integrity whitelist while rejecting valid memory-scope shapes",
        );
        assert!(
            matches!(
                ScopeNamespace::from_action("agent.register"),
                Some(ScopeNamespace::Agent)
            ),
            "agent.* prefix must route to ScopeNamespace::Agent",
        );
        assert!(
            matches!(
                ScopeNamespace::from_action("a2a.send"),
                Some(ScopeNamespace::A2a)
            ),
            "a2a.* prefix must route to ScopeNamespace::A2a — a swap with the tool arm would silently let a2a grants pass tool-scope validation and route the dispatch to the wrong runtime handler",
        );
        assert!(
            matches!(
                ScopeNamespace::from_action("audit.verify"),
                Some(ScopeNamespace::Audit)
            ),
            "audit.* prefix must route to ScopeNamespace::Audit",
        );
        assert!(
            matches!(
                ScopeNamespace::from_action("peers.list"),
                Some(ScopeNamespace::Peers)
            ),
            "peers.* prefix must route to ScopeNamespace::Peers — the peer validator's pubkey_b58/before_ms whitelist is materially different from any other namespace",
        );
        assert!(
            matches!(
                ScopeNamespace::from_action("identity.attest"),
                Some(ScopeNamespace::Identity)
            ),
            "identity.* prefix must route to ScopeNamespace::Identity — a refactor that dropped this arm during an 'identity-to-attestation' rename would silently route identity.* grants through the unknown-action fallthrough and bypass per-namespace validation",
        );
        assert!(
            matches!(
                ScopeNamespace::from_action("chain.flush"),
                Some(ScopeNamespace::Chain)
            ),
            "chain.* prefix must route to ScopeNamespace::Chain — the chain validator carries the load-bearing resource/mint/cluster/payer_pubkey_b58 contract for settlement audit",
        );

        // Unknown-action fallthrough: an action without any documented
        // prefix must return None so validate_scope can pass it
        // through the no-op path (intent.* and agent.* validators
        // also use the no-op path but only after the namespace check
        // routes them there).
        assert!(
            ScopeNamespace::from_action("madeup.action").is_none(),
            "unknown action prefix must return None so validate_scope passes the grant through to the no-op path; a refactor that defaulted unknown actions to a specific namespace would silently subject every opaque action to that namespace's validator",
        );
        assert!(
            ScopeNamespace::from_action("").is_none(),
            "empty action string must return None — the empty string does not start with any documented prefix; a refactor that gave None a specific-namespace default would surface a phantom routing here",
        );

        // Substring-vs-prefix contract: a documented namespace
        // appearing as a SUBSTRING (not a prefix) must NOT route.
        // The function uses str::starts_with, not str::contains.
        assert!(
            ScopeNamespace::from_action("a2a.tool.call").is_none()
                || matches!(
                    ScopeNamespace::from_action("a2a.tool.call"),
                    Some(ScopeNamespace::A2a)
                ),
            "a2a.tool.call starts with 'a2a.', so it routes to A2a — not Tool — even though 'tool.' appears as a substring; pinning that the FIRST documented prefix wins (no later substring match overrides) anchors the strict-prefix contract against a substring-relaxation refactor",
        );
        assert!(
            matches!(
                ScopeNamespace::from_action("a2a.tool.call"),
                Some(ScopeNamespace::A2a)
            ),
            "a2a.tool.call must route to A2a (the prefix-match arm that fires first) — a refactor that changed starts_with to contains would route this string to Tool and silently misvalidate a2a grants under tool-scope rules",
        );
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
    fn validate_peer_scope_rejects_invalid_self_and_before_ms_shapes() {
        assert_invalid_scope(
            "peers.revoke",
            serde_json::json!({ "version": 1, "self": "yes" }),
        );
        assert_invalid_scope(
            "peers.revoke",
            serde_json::json!({ "version": 1, "before_ms": -1 }),
        );
    }

    #[test]
    fn validate_chain_scope_rejects_invalid_mint_and_cluster_shapes() {
        assert_invalid_scope(
            "chain.flush",
            serde_json::json!({ "version": 1, "mint": "" }),
        );
        assert_invalid_scope(
            "chain.flush",
            serde_json::json!({ "version": 1, "cluster": "" }),
        );
        assert_invalid_scope(
            "chain.flush",
            serde_json::json!({ "version": 1, "cluster": 42 }),
        );
    }

    #[test]
    fn validate_chain_scope_rejects_unsupported_resource_value() {
        // validate_chain_scope binds the resource field to
        // optional_string_enum_or_null with the allowed list
        // ['compute', 'memory', 'tool', 'message', 'registration'].
        // The optional_string_enum_or_null helper test pins the
        // enum-miss behavior in isolation, but a regression at the
        // validate_chain_scope call site that relaxed the allowed list
        // (e.g., dropped 'registration', added 'cpu' as an alias for
        // 'compute', or merged categories) would not be caught by the
        // helper test alone — it only verifies the helper rejects an
        // unsupported value when given the documented allowed list.
        // validate_scope_accepts_known_versioned_shapes pins the
        // chain.flush happy path with resource='memory' but does not
        // exercise the rejection arm. Pin the resource enum-miss
        // rejection at the call site so a relaxation of the allowed
        // list — silently authorizing a settlement-flow scope class
        // the daemon's downstream chain logic does not recognize — is
        // caught loud at grant time, not at the runtime dispatch
        // boundary where the rejection would split operator audit
        // trails into a 'granted but never works' state.
        assert_invalid_scope(
            "chain.flush",
            serde_json::json!({ "version": 1, "resource": "cpu" }),
        );
        assert_invalid_scope(
            "chain.flush",
            serde_json::json!({ "version": 1, "resource": "unknown" }),
        );
    }

    #[test]
    fn validate_audit_scope_rejects_invalid_before_ms_and_include_integrity_types() {
        assert_invalid_scope(
            "audit.verify",
            serde_json::json!({ "version": 1, "before_ms": "bad" }),
        );
        assert_invalid_scope(
            "audit.verify",
            serde_json::json!({ "version": 1, "include_integrity": "yes" }),
        );
    }

    #[test]
    fn validate_memory_scope_rejects_invalid_record_id_and_apply_shapes() {
        assert_invalid_scope(
            "memory.write",
            serde_json::json!({ "version": 1, "record_id": 42 }),
        );
        assert_invalid_scope(
            "memory.write",
            serde_json::json!({ "version": 1, "apply": "yes" }),
        );
    }

    #[test]
    fn validate_memory_scope_rejects_invalid_before_ms_shapes() {
        // validate_memory_scope binds before_ms to
        // optional_non_negative_integer_or_null. The helper test
        // optional_non_negative_integer_or_null_pins_absent_null_zero_positive_negative_and_non_integer
        // pins non-integer and negative rejection at the helper
        // level, but a regression at the validate_memory_scope call
        // site that swapped the helper (e.g., to
        // optional_string_or_null for ISO-8601 timestamp forward-compat,
        // or to optional_non_negative_integer dropping the or_null
        // variant) would not be caught by the helper test alone.
        // sibling call sites pin this arm: validate_audit_scope
        // pins before_ms='bad' at audit.verify and validate_peer_scope
        // pins before_ms=-1 at peers.revoke. Pin both shapes at the
        // memory.write call site to cross-bind both regression vectors.
        assert_invalid_scope(
            "memory.write",
            serde_json::json!({ "version": 1, "before_ms": "bad" }),
        );
        assert_invalid_scope(
            "memory.write",
            serde_json::json!({ "version": 1, "before_ms": -1 }),
        );
    }

    #[test]
    fn validate_a2a_scope_rejects_invalid_task_id_and_lease_id_shapes() {
        assert_invalid_scope(
            "a2a.requeue",
            serde_json::json!({ "version": 1, "task_id": 42 }),
        );
        assert_invalid_scope(
            "a2a.requeue",
            serde_json::json!({ "version": 1, "lease_id": 42 }),
        );
    }

    #[test]
    fn validate_a2a_scope_rejects_unsupported_duplicate_risk_and_non_string_peer_pubkey() {
        // validate_a2a_scope checks four optional fields:
        //   peer_pubkey_b58 (string-or-null), task_id (string-or-null),
        //   lease_id (string-or-null), duplicate_risk (string-enum from
        //   ['idempotent', 'operator-accepted', 'operator_accepted']).
        //
        // validate_a2a_scope_rejects_invalid_task_id_and_lease_id_shapes
        // pins the non-string rejection for task_id and lease_id, and
        // validate_scope_accepts_known_versioned_shapes pins the happy
        // path with duplicate_risk='idempotent'. But two rejection arms
        // at the validate_a2a_scope call site are not pinned: the
        // duplicate_risk enum-miss path (an unsupported value like
        // 'best_effort' or 'safe' must reject so a regression that
        // broadened the allowed list — silently authorizing unsupported
        // duplicate-handling on A2A repair flows the daemon mailbox
        // does not implement — is caught loud), and the peer_pubkey_b58
        // non-string rejection path (a regression that swapped
        // optional_string_or_null for an opaque-Value validator
        // wouldn't let arbitrary scope payloads through the cap-action
        // whitelist).
        assert_invalid_scope(
            "a2a.requeue",
            serde_json::json!({ "version": 1, "duplicate_risk": "best_effort" }),
        );
        assert_invalid_scope(
            "a2a.requeue",
            serde_json::json!({ "version": 1, "peer_pubkey_b58": 42 }),
        );
    }

    #[test]
    fn validate_tool_scope_rejects_non_string_tool_and_non_object_arguments() {
        assert_invalid_scope(
            "tool.call.echo",
            serde_json::json!({ "version": 1, "tool": 42 }),
        );
        assert_invalid_scope(
            "tool.call.echo",
            serde_json::json!({ "version": 1, "arguments": "bad" }),
        );
    }

    #[test]
    fn validate_tool_scope_rejects_tool_mismatch_with_action_suffix() {
        assert_invalid_scope(
            "tool.call.echo",
            serde_json::json!({ "version": 1, "tool": "search" }),
        );
    }

    #[test]
    fn validate_tool_scope_rejects_non_object_arguments_allow() {
        assert_invalid_scope(
            "tool.call.echo",
            serde_json::json!({ "version": 1, "arguments": { "allow": "bad" } }),
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
    fn tool_call_scope_allows_pins_action_mismatch_non_object_and_missing_arguments_paths() {
        let bound_scope = serde_json::json!({ "version": 1, "tool": "search" });
        assert!(!tool_call_scope_allows(
            "tool.call.search",
            &bound_scope,
            "echo",
            &serde_json::json!({})
        )
        .unwrap());

        assert!(!tool_call_scope_allows(
            "tool",
            &serde_json::json!([]),
            "echo",
            &serde_json::json!({})
        )
        .unwrap());

        let tool_only_scope = serde_json::json!({ "version": 1, "tool": "foo" });
        assert!(tool_call_scope_allows(
            "tool.call.foo",
            &tool_only_scope,
            "foo",
            &serde_json::json!({})
        )
        .unwrap());
        assert!(tool_call_scope_allows(
            "tool.call.foo",
            &tool_only_scope,
            "foo",
            &serde_json::json!({ "any": "value" })
        )
        .unwrap());

        let args_without_allow =
            serde_json::json!({ "version": 1, "tool": "foo", "arguments": {} });
        assert!(tool_call_scope_allows(
            "tool.call.foo",
            &args_without_allow,
            "foo",
            &serde_json::json!({})
        )
        .unwrap());
        assert!(tool_call_scope_allows(
            "tool.call.foo",
            &args_without_allow,
            "foo",
            &serde_json::json!({ "any": "value" })
        )
        .unwrap());
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
    fn audit_purge_scope_allows_pins_non_object_action_mismatch_with_bound_scope_and_explicit_null_before_ms(
    ) {
        assert!(!audit_purge_scope_allows("audit", &serde_json::json!([]), 1_000).unwrap());

        let bound_scope = serde_json::json!({ "version": 1, "before_ms": 1_000 });
        assert!(!audit_purge_scope_allows("audit.verify", &bound_scope, 1_000).unwrap());

        let explicit_null = serde_json::json!({ "version": 1, "before_ms": null });
        assert!(audit_purge_scope_allows("audit.purge", &explicit_null, 0).unwrap());
        assert!(audit_purge_scope_allows("audit.purge", &explicit_null, u64::MAX).unwrap());
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
    fn capabilities_purge_scope_allows_pins_non_object_action_mismatch_with_bound_scope_and_explicit_null_before_ms(
    ) {
        assert!(!capabilities_purge_scope_allows(
            "capabilities.purge",
            &serde_json::json!([]),
            1_000
        )
        .unwrap());

        let bound_scope = serde_json::json!({ "before_ms": 1_000 });
        assert!(
            !capabilities_purge_scope_allows("capabilities.list", &bound_scope, 1_000).unwrap()
        );

        let explicit_null = serde_json::json!({ "before_ms": null });
        assert!(capabilities_purge_scope_allows("capabilities.purge", &explicit_null, 0).unwrap());
        assert!(
            capabilities_purge_scope_allows("capabilities.purge", &explicit_null, u64::MAX)
                .unwrap()
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
    fn a2a_scope_allows_pins_non_object_scope_and_validate_scope_error_propagation() {
        let request = A2aScopeRequest::default();
        assert!(
            !a2a_scope_allows("a2a", &serde_json::json!([]), "a2a.send.peer", request).unwrap()
        );

        let err = a2a_scope_allows(
            "a2a.send.peer",
            &serde_json::json!({ "version": 0 }),
            "a2a.send.peer",
            request,
        )
        .unwrap_err();
        assert!(matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("version")));
    }

    #[test]
    fn a2a_scope_allows_pins_peer_pubkey_b58_actual_none_with_bound_scope_rejected() {
        let scope = serde_json::json!({ "version": 1, "peer_pubkey_b58": "peer-1" });
        let actual_none = A2aScopeRequest {
            peer_pubkey_b58: None,
            ..A2aScopeRequest::default()
        };
        assert!(!a2a_scope_allows("a2a.send.peer", &scope, "a2a.send.peer", actual_none).unwrap());
    }

    #[test]
    fn a2a_scope_allows_pins_duplicate_risk_actual_none_with_bound_scope_rejected() {
        let scope = serde_json::json!({ "version": 1, "duplicate_risk": "idempotent" });
        let actual_none = A2aScopeRequest {
            duplicate_risk: None,
            ..A2aScopeRequest::default()
        };
        assert!(!a2a_scope_allows("a2a.requeue", &scope, "a2a.requeue", actual_none).unwrap());
    }

    #[test]
    fn a2a_scope_allows_pins_duplicate_risk_underscore_dash_normalization() {
        let underscore_scope =
            serde_json::json!({ "version": 1, "duplicate_risk": "operator_accepted" });
        let dash_actual = A2aScopeRequest {
            duplicate_risk: Some("operator-accepted"),
            ..A2aScopeRequest::default()
        };
        assert!(
            a2a_scope_allows("a2a.requeue", &underscore_scope, "a2a.requeue", dash_actual).unwrap()
        );

        let dash_scope = serde_json::json!({ "version": 1, "duplicate_risk": "operator-accepted" });
        let underscore_actual = A2aScopeRequest {
            duplicate_risk: Some("operator_accepted"),
            ..A2aScopeRequest::default()
        };
        assert!(
            a2a_scope_allows("a2a.requeue", &dash_scope, "a2a.requeue", underscore_actual).unwrap()
        );
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
    fn peer_scope_allows_pins_non_object_scope_and_validate_scope_error_propagation() {
        let request = PeerScopeRequest::default();
        assert!(
            !peer_scope_allows("peers", &serde_json::json!([]), "peers.revoke", request).unwrap()
        );

        let err = peer_scope_allows(
            "peers.revoke",
            &serde_json::json!({ "version": 0 }),
            "peers.revoke",
            request,
        )
        .unwrap_err();
        assert!(matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("version")));
    }

    #[test]
    fn peer_scope_allows_pins_token_prefix_actual_none_rejected() {
        let scope = serde_json::json!({ "version": 1, "token_prefix": "abc" });
        let actual_none = PeerScopeRequest {
            token_prefix: None,
            ..PeerScopeRequest::default()
        };
        assert!(!peer_scope_allows("peers.revoke", &scope, "peers.revoke", actual_none).unwrap());
    }

    #[test]
    fn peer_scope_allows_pins_before_ms_equal_boundary_accepted() {
        let scope = serde_json::json!({ "version": 1, "before_ms": 1_000 });
        let equal = PeerScopeRequest {
            before_ms: Some(1_000),
            ..PeerScopeRequest::default()
        };
        assert!(peer_scope_allows("peers.purge", &scope, "peers.purge", equal).unwrap());

        let beyond = PeerScopeRequest {
            before_ms: Some(1_001),
            ..PeerScopeRequest::default()
        };
        assert!(!peer_scope_allows("peers.purge", &scope, "peers.purge", beyond).unwrap());
    }

    #[test]
    fn peer_scope_allows_pins_before_ms_actual_none_with_bound_scope_rejected() {
        let scope = serde_json::json!({ "version": 1, "before_ms": 1_000 });
        let actual_none = PeerScopeRequest {
            before_ms: None,
            ..PeerScopeRequest::default()
        };
        assert!(!peer_scope_allows("peers.purge", &scope, "peers.purge", actual_none).unwrap());
    }

    #[test]
    fn peer_scope_allows_pins_self_actual_none_with_bound_scope_rejected() {
        let scope = serde_json::json!({ "version": 1, "self": false });
        let actual_none = PeerScopeRequest {
            self_target: None,
            ..PeerScopeRequest::default()
        };
        assert!(!peer_scope_allows("peers.revoke", &scope, "peers.revoke", actual_none).unwrap());
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
    fn chain_scope_allows_pins_non_object_scope_and_validate_scope_error_propagation() {
        let request = ChainScopeRequest::default();
        assert!(
            !chain_scope_allows("chain", &serde_json::json!([]), "chain.flush", request).unwrap()
        );

        let err = chain_scope_allows(
            "chain.flush",
            &serde_json::json!({ "version": 0 }),
            "chain.flush",
            request,
        )
        .unwrap_err();
        assert!(matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("version")));
    }

    #[test]
    fn chain_scope_allows_pins_optional_string_actual_none_accepted() {
        let scope = serde_json::json!({ "version": 1, "resource": "compute" });
        let actual_none = ChainScopeRequest {
            resource: None,
            ..ChainScopeRequest::default()
        };
        assert!(chain_scope_allows("chain.flush", &scope, "chain.flush", actual_none).unwrap());
    }

    #[test]
    fn chain_scope_allows_pins_limit_equal_boundary_accepted() {
        let scope = serde_json::json!({ "version": 1, "limit": 10 });
        let equal = ChainScopeRequest {
            limit: Some(10),
            ..ChainScopeRequest::default()
        };
        assert!(chain_scope_allows("chain.flush", &scope, "chain.flush", equal).unwrap());

        let beyond = ChainScopeRequest {
            limit: Some(11),
            ..ChainScopeRequest::default()
        };
        assert!(!chain_scope_allows("chain.flush", &scope, "chain.flush", beyond).unwrap());
    }

    #[test]
    fn chain_scope_allows_pins_limit_actual_none_with_bound_scope_accepted() {
        let scope = serde_json::json!({ "version": 1, "limit": 10 });
        let actual_none = ChainScopeRequest {
            limit: None,
            ..ChainScopeRequest::default()
        };
        assert!(chain_scope_allows("chain.flush", &scope, "chain.flush", actual_none).unwrap());
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
    fn memory_purge_scope_allows_pins_non_object_action_mismatch_with_bound_scope_and_unscoped_grants(
    ) {
        assert!(
            !memory_purge_scope_allows("memory", &serde_json::json!([]), Some("working"), 0)
                .unwrap()
        );

        let bound_scope = serde_json::json!({
            "version": 1,
            "tiers": ["working"],
            "before_ms": 1_000,
            "apply": true
        });
        assert!(
            !memory_purge_scope_allows("memory.read", &bound_scope, Some("working"), 999).unwrap()
        );

        let empty = serde_json::json!({});
        assert!(memory_purge_scope_allows("memory.purge", &empty, Some("working"), 0).unwrap());
        assert!(memory_purge_scope_allows("memory.purge", &empty, None, u64::MAX).unwrap());
    }

    #[test]
    fn memory_purge_scope_allows_pins_before_ms_equal_boundary_accepted() {
        let scope = serde_json::json!({
            "version": 1,
            "tiers": ["working"],
            "before_ms": 1_000,
            "apply": true
        });
        assert!(memory_purge_scope_allows("memory.purge", &scope, Some("working"), 1_000).unwrap());
        assert!(
            !memory_purge_scope_allows("memory.purge", &scope, Some("working"), 1_001).unwrap()
        );
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
    fn memory_read_scope_allows_pins_non_object_unscoped_and_tier_none_with_bound_scope() {
        assert!(
            !memory_read_scope_allows("memory", &serde_json::json!([]), Some("working")).unwrap()
        );

        let empty = serde_json::json!({});
        assert!(memory_read_scope_allows("memory.read", &empty, Some("working")).unwrap());
        assert!(memory_read_scope_allows("memory.read", &empty, None).unwrap());

        let tier_bound = serde_json::json!({ "version": 1, "tiers": ["working"] });
        assert!(memory_read_scope_allows("memory.read", &tier_bound, None).unwrap());
    }

    #[test]
    fn memory_read_record_scope_allows_pins_action_mismatch_non_object_and_explicit_null_before_ms()
    {
        let bound_scope = serde_json::json!({ "version": 1, "tiers": ["working"] });
        assert!(!memory_read_record_scope_allows(
            "memory.read.short",
            &bound_scope,
            "record-1",
            "long",
            0
        )
        .unwrap());

        assert!(!memory_read_record_scope_allows(
            "memory",
            &serde_json::json!([]),
            "record-1",
            "working",
            0
        )
        .unwrap());

        let explicit_null = serde_json::json!({ "version": 1, "before_ms": null });
        assert!(memory_read_record_scope_allows(
            "memory.read",
            &explicit_null,
            "record-1",
            "working",
            0
        )
        .unwrap());
        assert!(memory_read_record_scope_allows(
            "memory.read",
            &explicit_null,
            "record-1",
            "working",
            u64::MAX
        )
        .unwrap());
    }

    #[test]
    fn memory_read_record_scope_allows_pins_before_ms_equal_boundary_rejected() {
        let scope = serde_json::json!({
            "version": 1,
            "apply": false,
            "tiers": ["working"],
            "record_id": "rec-1",
            "before_ms": 1_000
        });
        assert!(!memory_read_record_scope_allows(
            "memory.read.working",
            &scope,
            "rec-1",
            "working",
            1_000
        )
        .unwrap());
        assert!(memory_read_record_scope_allows(
            "memory.read.working",
            &scope,
            "rec-1",
            "working",
            999
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
    fn memory_write_scope_allows_pins_non_object_action_mismatch_with_bound_scope_and_explicit_null_before_ms(
    ) {
        assert!(!memory_write_scope_allows(
            "memory",
            &serde_json::json!([]),
            "record-1",
            "working",
            0
        )
        .unwrap());

        let bound_scope = serde_json::json!({
            "version": 1,
            "tiers": ["working"],
            "apply": true
        });
        assert!(
            !memory_write_scope_allows("memory.read", &bound_scope, "record-1", "working", 0)
                .unwrap()
        );

        let explicit_null = serde_json::json!({
            "version": 1,
            "apply": true,
            "before_ms": null,
        });
        assert!(memory_write_scope_allows(
            "memory.write",
            &explicit_null,
            "record-1",
            "working",
            0
        )
        .unwrap());
        assert!(memory_write_scope_allows(
            "memory.write",
            &explicit_null,
            "record-1",
            "working",
            u64::MAX
        )
        .unwrap());
    }

    #[test]
    fn memory_write_scope_allows_pins_before_ms_equal_boundary_rejected() {
        let scope = serde_json::json!({
            "version": 1,
            "apply": true,
            "before_ms": 1_000
        });
        assert!(
            !memory_write_scope_allows("memory.write", &scope, "record-1", "working", 1_000)
                .unwrap()
        );
        assert!(
            memory_write_scope_allows("memory.write", &scope, "record-1", "working", 999).unwrap()
        );
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
    fn memory_repair_scope_allows_pins_non_object_action_mismatch_with_bound_scope_and_unscoped_grants(
    ) {
        assert!(!memory_repair_scope_allows(
            "memory",
            &serde_json::json!([]),
            "record-1",
            "working",
            0,
            true
        )
        .unwrap());

        let bound_scope = serde_json::json!({
            "version": 1,
            "record_id": "record-1",
            "tiers": ["working"],
            "before_ms": 1_000,
            "apply": true
        });
        assert!(!memory_repair_scope_allows(
            "memory.repair.dry_run",
            &bound_scope,
            "record-1",
            "working",
            999,
            true
        )
        .unwrap());

        let empty = serde_json::json!({});
        assert!(memory_repair_scope_allows(
            "memory.repair.apply",
            &empty,
            "any-record",
            "working",
            0,
            true
        )
        .unwrap());
        assert!(memory_repair_scope_allows(
            "memory.repair.apply",
            &empty,
            "any-record",
            "longterm",
            u64::MAX,
            true
        )
        .unwrap());
    }

    #[test]
    fn memory_repair_scope_allows_pins_validate_scope_version_error_propagation() {
        let err = memory_repair_scope_allows(
            "memory.repair.apply",
            &serde_json::json!({ "version": 0 }),
            "record-1",
            "working",
            0,
            true,
        )
        .unwrap_err();
        assert!(matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("version")));
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
    fn memory_compaction_scope_allows_pins_non_object_action_mismatch_with_bound_scope_and_unscoped_grants(
    ) {
        assert!(!memory_compaction_scope_allows(
            "memory",
            &serde_json::json!([]),
            MemoryCompactionScopeRequest {
                apply: true,
                delete_working_before_ms: None,
                delete_episodic_before_ms: None,
                mark_longterm_stale_before_ms: None,
                detach_stale_parents: false,
            }
        )
        .unwrap());

        let bound_scope = serde_json::json!({
            "version": 1,
            "tiers": ["working", "episodic"],
            "before_ms": 1_000,
            "apply": true
        });
        assert!(!memory_compaction_scope_allows(
            "memory.compact.dry_run",
            &bound_scope,
            MemoryCompactionScopeRequest {
                apply: true,
                delete_working_before_ms: Some(999),
                delete_episodic_before_ms: None,
                mark_longterm_stale_before_ms: None,
                detach_stale_parents: false,
            }
        )
        .unwrap());

        let empty = serde_json::json!({});
        assert!(memory_compaction_scope_allows(
            "memory.compact.apply",
            &empty,
            MemoryCompactionScopeRequest {
                apply: true,
                delete_working_before_ms: Some(u64::MAX),
                delete_episodic_before_ms: Some(u64::MAX),
                mark_longterm_stale_before_ms: Some(u64::MAX),
                detach_stale_parents: true,
            }
        )
        .unwrap());
        assert!(memory_compaction_scope_allows(
            "memory.compact.apply",
            &empty,
            MemoryCompactionScopeRequest {
                apply: true,
                delete_working_before_ms: None,
                delete_episodic_before_ms: None,
                mark_longterm_stale_before_ms: None,
                detach_stale_parents: false,
            }
        )
        .unwrap());
    }

    #[test]
    fn memory_compaction_scope_allows_pins_validate_scope_version_error_propagation() {
        let err = memory_compaction_scope_allows(
            "memory.compact.apply",
            &serde_json::json!({ "version": 0 }),
            MemoryCompactionScopeRequest {
                apply: true,
                delete_working_before_ms: None,
                delete_episodic_before_ms: None,
                mark_longterm_stale_before_ms: None,
                detach_stale_parents: false,
            },
        )
        .unwrap_err();
        assert!(matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("version")));
    }

    #[test]
    fn memory_compaction_scope_allows_pins_per_tier_cutoff_tier_binding() {
        let scope = serde_json::json!({
            "version": 1,
            "tiers": ["working"],
            "before_ms": 1_000,
            "apply": true
        });
        assert!(!memory_compaction_scope_allows(
            "memory.compact.apply",
            &scope,
            MemoryCompactionScopeRequest {
                apply: true,
                delete_working_before_ms: None,
                delete_episodic_before_ms: Some(500),
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
                mark_longterm_stale_before_ms: Some(500),
                detach_stale_parents: false,
            }
        )
        .unwrap());
    }

    #[test]
    fn memory_compaction_scope_allows_pins_detach_stale_parents_tiers_key_conditional() {
        let request = MemoryCompactionScopeRequest {
            apply: true,
            delete_working_before_ms: None,
            delete_episodic_before_ms: None,
            mark_longterm_stale_before_ms: None,
            detach_stale_parents: true,
        };

        let no_tiers = serde_json::json!({
            "version": 1,
            "apply": true
        });
        assert!(
            memory_compaction_scope_allows("memory.compact.apply", &no_tiers, request).unwrap()
        );

        let all_tiers = serde_json::json!({
            "version": 1,
            "tiers": ["working", "episodic", "longterm"],
            "apply": true
        });
        assert!(
            memory_compaction_scope_allows("memory.compact.apply", &all_tiers, request).unwrap()
        );

        let partial_tiers = serde_json::json!({
            "version": 1,
            "tiers": ["working", "episodic"],
            "apply": true
        });
        assert!(
            !memory_compaction_scope_allows("memory.compact.apply", &partial_tiers, request)
                .unwrap()
        );
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
    fn canonical_message_pins_exact_byte_layout_for_known_fixture() {
        // canonical_message (line 986-1002) is the byte-level signed-
        // message blob that ed25519 signing and verification operate
        // on. Its layout is the cryptographic anchor for every
        // SignedCapability written to disk or transmitted over IPC:
        //
        //   [32 bytes: subject.pubkey]
        //   [4 bytes: action length, u32 big-endian]
        //   [N bytes: action UTF-8]
        //   [4 bytes: scope length, u32 big-endian]
        //   [M bytes: scope JCS-encoded JSON]
        //   [32 bytes: granted_by.pubkey]
        //   [1 byte: expires_at presence flag — 1 for Some, 0 for None]
        //   [8 bytes: expires_at value, u64 big-endian — 0 when None]
        //
        // The existing canonical_message_is_jcs_stable_across_scope_key_orderings
        // pin (just below this test) covers JCS canonicalisation of
        // the scope field but does NOT pin the surrounding byte
        // format. A refactor that flipped the length prefixes from
        // u32 BE to u32 LE, switched the expires_at encoding from
        // flag-then-value to a u64::MAX sentinel for None, reordered
        // the fields (e.g., put granted_by before scope), or shifted
        // any field by even one byte would silently invalidate every
        // existing signed capability — sign/verify pairs in unit
        // tests still pass because both sides use the new layout,
        // but operator-issued grants written under the prior daemon
        // version stop verifying with BadSignature on the next
        // restart and the operator has no parse-time or compile-time
        // signal that the layout drifted.
        let none_cap = Capability {
            subject: AgentId::new("subject@local", [0u8; 32]),
            action: "x".into(),
            scope: serde_json::json!({}),
            granted_by: AgentId::new("granter@local", [0u8; 32]),
            expires_at: None,
        };
        let none_msg = canonical_message(&none_cap);

        let mut expected = Vec::new();
        expected.extend_from_slice(&[0u8; 32]);
        expected.extend_from_slice(&1u32.to_be_bytes());
        expected.push(b'x');
        expected.extend_from_slice(&2u32.to_be_bytes());
        expected.extend_from_slice(b"{}");
        expected.extend_from_slice(&[0u8; 32]);
        expected.push(0);
        expected.extend_from_slice(&0u64.to_be_bytes());

        assert_eq!(
            none_msg.len(),
            84,
            "canonical_message must emit exactly 84 bytes for the \
             known fixture (32 + 4 + 1 + 4 + 2 + 32 + 1 + 8); a \
             refactor that shifted any field, swapped any length \
             prefix width (e.g., u32 → u16 → u64), or added a new \
             field would change this total and silently invalidate \
             every persisted SignedCapability; got len {}",
            none_msg.len(),
        );
        assert_eq!(
            none_msg, expected,
            "canonical_message must produce the documented byte \
             sequence verbatim for the all-zero-pubkey, action='x', \
             empty-scope, expires_at-None fixture; the prefix \
             encoding is u32 big-endian (network byte order), the \
             field order is subject|action|scope|granter|expires, \
             and the expires_at-None case emits flag-byte 0 followed \
             by 8 zero bytes for the value; a refactor that flipped \
             the prefixes to little-endian, reordered the fields, \
             or changed the expires_at-None encoding to a sentinel \
             (e.g., u64::MAX) would break verify() on every signed \
             capability written under the prior daemon version with \
             no parse-time signal",
        );

        let some_cap = Capability {
            subject: AgentId::new("subject@local", [0u8; 32]),
            action: "x".into(),
            scope: serde_json::json!({}),
            granted_by: AgentId::new("granter@local", [0u8; 32]),
            expires_at: Some(0x0102_0304_0506_0708),
        };
        let some_msg = canonical_message(&some_cap);

        assert_eq!(
            some_msg.len(),
            84,
            "expires_at-Some must keep the same total length as the \
             expires_at-None case (the flag-then-value encoding uses \
             1 + 8 bytes in both arms); a refactor that omitted the \
             flag byte when expires_at is Some would shrink the \
             total to 83 bytes and the asymmetric layout would \
             break verify() on every time-bound grant",
        );
        let tail_start = some_msg.len() - 9;
        assert_eq!(
            some_msg[tail_start], 1,
            "expires_at-Some must flip the presence flag byte from \
             0 to 1; the rest of the blob is identical to the None \
             case, so the only signal that the capability is \
             time-bound is this one byte — a refactor that dropped \
             the flag would let absent/present expiry blur together \
             and verify_with_clock would silently treat None-encoded \
             caps as Some(0) (always expired)",
        );
        assert_eq!(
            &some_msg[tail_start + 1..],
            &0x0102_0304_0506_0708u64.to_be_bytes(),
            "expires_at-Some must encode the value as u64 big-endian \
             (network byte order) in the trailing 8 bytes — a \
             refactor to little-endian would flip the byte order and \
             every persisted signed capability with a non-trivial \
             expires_at would become BadSignature on restart, while \
             expires_at=0 caps (which round-trip identically under \
             either endianness) would still verify, masking the \
             regression to time-bound grants only",
        );

        for (idx, (none_byte, some_byte)) in none_msg
            .iter()
            .zip(some_msg.iter())
            .take(tail_start)
            .enumerate()
        {
            assert_eq!(
                none_byte, some_byte,
                "the expires_at-None and expires_at-Some blobs must \
                 share the leading 75 bytes (subject, action, scope, \
                 granter) verbatim; only the trailing 9 bytes \
                 (flag + value) differ — divergence at byte {idx} \
                 would mean a non-expires field is implicated, \
                 which violates the documented layout",
            );
        }
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

    #[test]
    fn verify_with_clock_pins_expires_at_equal_boundary_accepted() {
        // verify_with_clock (line 1029-1037) is the only API the daemon
        // calls to check capability expiry at dispatch time. The check is
        //
        //     if let Some(exp) = signed.capability.expires_at {
        //         if now_ms > exp {
        //             return Err(PermissionError::Expired(exp));
        //         }
        //     }
        //
        // — a STRICT greater-than that documents 'a capability with
        // expires_at = T is valid through and including the wall-clock
        // instant T (in epoch ms); it becomes Expired one millisecond
        // later at T+1'.
        //
        // verify_with_clock_rejects_expired (above) covers the BEFORE arm
        // (now_ms = 999 < exp = 1000 → Ok) and the AFTER arm (now_ms =
        // 1001 > exp = 1000 → Expired(1000)) but never asserts the EQUAL
        // arm (now_ms = 1000 == exp = 1000 → Ok). A refactor that
        // swapped `>` for `>=` under a 'use canonical exclusive-upper-
        // bound semantics' or 'half-open interval [valid_from,
        // expires_at)' pass would silently shift every capability's
        // effective expiry one millisecond earlier — caps with
        // expires_at = T would start rejecting at exactly T instead of
        // at T+1. The before/after arms in verify_with_clock_rejects_expired
        // would still pass under either operator because they probe 999
        // and 1001, not 1000.

        let issuer = LocalIdentity::generate("authority@local");
        let subject = LocalIdentity::generate("research@local").agent_id();
        let signed = sign(
            cap(
                subject.clone(),
                "tool.web_search",
                issuer.agent_id(),
                Some(1000),
            ),
            issuer.signing_key(),
        );

        // (1) Equality arm — the new pin. now_ms == expires_at must
        // accept because the operator is strict greater-than. This is
        // the minimum-distance probe of the `>` vs `>=` regression.
        assert!(
            verify_with_clock(&signed, 1000).is_ok(),
            "now_ms == expires_at must verify as Ok — the operator is \
             'if now_ms > exp', strictly greater-than, so the equality \
             case is valid. A refactor that flipped to `>=` would \
             silently reject every capability at exactly its expiry \
             timestamp; rotation policies that issue a replacement cap \
             with valid_from == old.expires_at would see a one-millisecond \
             coverage gap at every rotation",
        );

        // (2) None-expiry arm. A cap with expires_at = None must accept
        // at any clock value — the documented contract is 'no expiry'.
        // A refactor that flattened the Option match to
        // `.unwrap_or(0)` (the natural u64 default) would treat None as
        // exp=0 and silently reject every None-expiry cap as
        // Expired(0). u64::MAX is the only clock value that
        // distinguishes the correct branched implementation from a
        // .unwrap_or(0) regression while also surfacing a hypothetical
        // .unwrap_or(u64::MAX) variant (which would shift behavior on
        // any clock-skew tolerance refactor).
        let no_expiry = sign(
            cap(subject, "tool.web_search", issuer.agent_id(), None),
            issuer.signing_key(),
        );
        assert!(
            verify_with_clock(&no_expiry, u64::MAX).is_ok(),
            "expires_at = None must verify as Ok at any clock value — \
             a refactor that simplified the Option<u64> match to \
             '.unwrap_or(0)' would silently reject every None-expiry \
             capability as Expired(0); the audit log would surface \
             these rejections with no signal that the Option handling \
             drifted",
        );

        // (3) Cross-bind error-value arm. Duplicates the existing
        // verify_with_clock_rejects_expired assertion to anchor the
        // exact value wrapped in the Expired variant: the cap's
        // expires_at, NOT now_ms. A refactor that changed
        // Err(Expired(exp)) to Err(Expired(now_ms)) under a 'report
        // rejection-time clock for forensics' rationale would silently
        // break operator dashboards that key on the cap's stated
        // expires_at to correlate alerts with the granted_capabilities
        // ledger.
        assert!(
            matches!(
                verify_with_clock(&signed, 1001),
                Err(PermissionError::Expired(1000))
            ),
            "rejection error must wrap the cap's expires_at (1000), \
             not now_ms (1001); a refactor that swapped the wrapped \
             value would silently decorrelate audit-log alerts from \
             the granted_capabilities ledger entries operators reconcile \
             against",
        );
    }

    #[test]
    fn verify_with_clock_and_trust_root_pins_trust_root_boundary() {
        let issuer = LocalIdentity::generate("authority@local");
        let stranger = LocalIdentity::generate("stranger@local");
        let subject = LocalIdentity::generate("research@local").agent_id();
        let signed = sign(
            cap(subject, "tool.web_search", issuer.agent_id(), Some(1000)),
            issuer.signing_key(),
        );

        // (a) granted_by matches trust_root and capability is unexpired.
        assert!(verify_with_clock_and_trust_root(&signed, 999, issuer.pubkey_bytes()).is_ok());

        // (b) trust_root mismatch with an otherwise valid capability is
        // rejected as UntrustedGrantor, not BadSignature or Expired.
        assert!(matches!(
            verify_with_clock_and_trust_root(&signed, 999, stranger.pubkey_bytes()),
            Err(PermissionError::UntrustedGrantor)
        ));

        // (c) trust_root check happens before signature verification: a
        // tampered signature from an untrusted grantor must still surface
        // as UntrustedGrantor (not BadSignature) so the trust-root gate
        // cannot be masked by a later failure path.
        let mut tampered = signed.clone();
        tampered.signature = [0u8; 64];
        assert!(matches!(
            verify_with_clock_and_trust_root(&tampered, 999, stranger.pubkey_bytes()),
            Err(PermissionError::UntrustedGrantor)
        ));

        // (d) when granted_by matches trust_root, the Expired error from
        // verify_with_clock still propagates through this wrapper.
        assert!(matches!(
            verify_with_clock_and_trust_root(&signed, 1001, issuer.pubkey_bytes()),
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

    #[test]
    fn signed_capability_envelope_serde_pins_two_required_fields() {
        // SignedCapability is the signed grant envelope that rides
        // every JSONL grant log line, IPC capability response, and HTTP
        // grant surface. Two fields: capability (the inner Capability
        // struct, pinned separately by capability_serde_pins_five_field_wire_form)
        // and signature ([u8; 64] serialized as bs58 via
        // #[serde(with = "sig_b58")], pinned by
        // signed_capability_signature_serde_pins_base58_and_length_arms).
        // The existing round-trip test
        // signed_capability_round_trips_through_serde is too loose to
        // detect a refactor that flattens the inner Capability into the
        // envelope, or adds a third top-level metadata field, or renames
        // either documented key. Pin the envelope key set on the wire,
        // the per-required-field reject, and the no-flatten contract.
        let issuer = LocalIdentity::generate("authority@local");
        let subject = LocalIdentity::generate("research@local").agent_id();
        let signed = sign(
            cap(subject, "memory.write", issuer.agent_id(), None),
            issuer.signing_key(),
        );

        let wire = serde_json::to_value(&signed).unwrap();
        let obj = wire
            .as_object()
            .expect("SignedCapability serializes as a JSON object");
        let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        let expected: std::collections::BTreeSet<&str> =
            ["capability", "signature"].into_iter().collect();
        assert_eq!(
            keys, expected,
            "SignedCapability wire form must be exactly two top-level keys; \
             a #[serde(flatten)] on capability would lift inner fields to \
             the envelope and silently collide with any future metadata \
             sibling, and an added top-level field would break the JSONL \
             grant log replay round-trip",
        );

        let capability_value = obj
            .get("capability")
            .expect("capability key must be present");
        assert!(
            capability_value.is_object(),
            "capability must serialise as a nested JSON object; a flatten \
             would surface inner Capability keys (subject, action, scope, …) \
             at the envelope level instead",
        );
        // The envelope must NOT surface any of the inner Capability keys
        // at the top level — that would mean capability was flattened.
        for inner in ["subject", "action", "scope", "granted_by", "expires_at"] {
            assert!(
                !obj.contains_key(inner),
                "envelope must not expose inner Capability key {inner:?} at \
                 the top level — that would mean capability was flattened",
            );
        }

        let signature_value = obj.get("signature").expect("signature key must be present");
        assert!(
            signature_value.is_string(),
            "signature must serialise as a JSON string (bs58); the contract \
             is pinned by signed_capability_signature_serde_pins_base58_and_length_arms",
        );

        // Each strictly-required field must reject when omitted.
        for required in ["capability", "signature"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<SignedCapability>(serde_json::Value::Object(missing))
                    .is_err(),
                "SignedCapability wire form must reject a payload missing {required:?}",
            );
        }

        // A payload that flattens Capability fields directly into the
        // envelope (no inner capability key) must fail — that's the
        // shape a refactor introducing #[serde(flatten)] on capability
        // would produce, and the deserializer must refuse it so the
        // regression fails loud at the envelope boundary.
        let inner_cap = serde_json::to_value(&signed.capability).unwrap();
        let mut flattened = inner_cap.as_object().unwrap().clone();
        flattened.insert("signature".into(), obj.get("signature").unwrap().clone());
        assert!(
            serde_json::from_value::<SignedCapability>(serde_json::Value::Object(flattened))
                .is_err(),
            "flattened capability fields (no inner capability key) must be \
             rejected so a #[serde(flatten)] refactor fails at the envelope",
        );
    }

    #[test]
    fn signed_capability_signature_serde_pins_base58_and_length_arms() {
        // SignedCapability::signature carries #[serde(with = "sig_b58")];
        // every JSONL grant log, IPC capability response, and HTTP grant
        // surface depends on the exact wire form. A refactor that swaps
        // base58 for base64/hex invalidates every persisted grant on
        // reopen; a refactor that drops the 64-byte length check silently
        // accepts malformed signatures the verifier cannot recompute.
        let issuer = LocalIdentity::generate("authority@local");
        let subject = LocalIdentity::generate("research@local").agent_id();
        let signed = sign(
            cap(subject, "memory.write", issuer.agent_id(), None),
            issuer.signing_key(),
        );

        let wire = serde_json::to_value(&signed).unwrap();
        let sig_str = wire
            .get("signature")
            .and_then(|v| v.as_str())
            .expect("signature field must be a JSON string named 'signature'; a rename or non-string encoding strands every JSONL grant log");
        assert!(
            !sig_str.is_empty(),
            "serialized signature must be a non-empty base58 string",
        );
        let decoded = bs58::decode(sig_str)
            .into_vec()
            .expect("signature wire form must be valid base58; a refactor to base64/hex invalidates every persisted grant");
        assert_eq!(
            decoded.len(),
            64,
            "decoded signature must be exactly 64 bytes; the base58 wire form is the only contract that lets the verifier recompute the ed25519 signature",
        );
        assert_eq!(
            decoded,
            signed.signature.to_vec(),
            "decoded signature bytes must equal the in-memory [u8; 64]; an encoding swap would silently corrupt every grant",
        );

        let short = bs58::encode([0u8; 32]).into_string();
        let payload_short = serde_json::json!({
            "capability": serde_json::to_value(&signed.capability).unwrap(),
            "signature": short,
        });
        assert!(
            serde_json::from_value::<SignedCapability>(payload_short).is_err(),
            "32-byte signature must be rejected; a dropped length check silently zero-pads malformed grants",
        );

        let payload_garbage = serde_json::json!({
            "capability": serde_json::to_value(&signed.capability).unwrap(),
            "signature": "!!! not base58 !!!",
        });
        assert!(
            serde_json::from_value::<SignedCapability>(payload_garbage).is_err(),
            "non-base58 signature string must be rejected so the decoder never silently zero-fills invalid grants",
        );

        let serialized = serde_json::to_string(&signed).unwrap();
        for byte in signed.signature.iter().take(4) {
            let escape = format!("\\u{byte:04x}");
            assert!(
                !serialized.contains(&escape),
                "serialized form must not contain raw signature bytes as escapes; the field must be base58-encoded, not raw",
            );
        }
    }

    #[test]
    fn revocation_serde_pins_two_required_fields() {
        // covenant_permissions::Revocation is the durable record the
        // daemon writes to revoked.jsonl per revoke() call; a
        // capability is treated as live iff its signature is in
        // granted.jsonl AND NOT in revoked.jsonl. The wire form is
        // exactly two top-level keys: 'signature' plus 'revoked_at'.
        //
        // signature is [u8; 64] with #[serde(with = "sig_b58")] —
        // base58 string on the wire, NOT a JSON array of 64 numbers,
        // NOT hex. The decoder enforces a strict 64-byte length and
        // rejects any other length. revoked_at is u64 (Unix ms
        // timestamp). Both fields are required — neither has
        // #[serde(default)].
        //
        // This slice locks: the exact two-key wire shape, the base58
        // string encoding of signature, the strict length-64
        // rejection on decode (a shorter or longer base58 string
        // must be rejected, NOT zero-padded), the rejection of a
        // JSON array of 64 numbers in the signature slot (the
        // custom serializer is string-based), and round-trip.
        //
        // The durable jsonl read path is
        // `Self::read_jsonl::<Revocation>(&self.revoked_path)`, so
        // a regression that swapped signature for a JSON array
        // would silently fail to deserialise every existing
        // revocation record on operator restart — silently
        // restoring 'unrevoked' status to every legitimate
        // revocation.
        let revocation = Revocation {
            signature: [7u8; 64],
            revoked_at: 1_700_000_000_000,
        };

        let wire = serde_json::to_value(&revocation).unwrap();
        let obj = wire
            .as_object()
            .expect("Revocation serializes as a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["revoked_at", "signature"],
            "Revocation wire form must be exactly two top-level \
             keys ('signature', 'revoked_at'). A refactor that \
             added a third field without bumping the durable \
             revoked.jsonl format would silently mismatch every \
             existing revocation record on operator restart",
        );

        let sig_str = obj
            .get("signature")
            .and_then(serde_json::Value::as_str)
            .expect(
                "Revocation::signature must surface as a JSON string \
                 (custom sig_b58 serializer); a refactor that dropped \
                 the #[serde(with = \"sig_b58\")] attribute would \
                 surface signature as a JSON array of 64 numbers and \
                 invalidate every persisted revoked.jsonl row on \
                 operator restart",
            );
        let expected_b58 = bs58::encode([7u8; 64]).into_string();
        assert_eq!(
            sig_str, expected_b58,
            "Revocation::signature wire form must equal \
             bs58::encode(signature_bytes); a refactor to base64 or \
             hex would invalidate every existing revoked.jsonl row \
             on operator restart and silently restore 'unrevoked' \
             status to every legitimate revocation",
        );

        assert_eq!(
            obj.get("revoked_at"),
            Some(&serde_json::json!(1_700_000_000_000u64)),
            "Revocation::revoked_at must surface as a JSON number \
             (Unix ms timestamp); a refactor that added \
             #[serde(skip_serializing_if)] or changed the type to a \
             string would silently break operator-driven retention \
             purges that key on revoked_at < before_ms",
        );

        let back: Revocation = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            back, revocation,
            "Revocation must round-trip through serde_json verbatim \
             — the PartialEq derive is the contract every \
             revoked.jsonl read path leans on",
        );

        let mut missing_signature = obj.clone();
        missing_signature.remove("signature");
        assert!(
            serde_json::from_value::<Revocation>(serde_json::Value::Object(missing_signature))
                .is_err(),
            "Revocation wire form must reject a payload missing \
             'signature' (no #[serde(default)] on the field) — a \
             relaxation would let untyped tombstones slip into \
             revoked.jsonl and silently restore 'unrevoked' status \
             to every existing revocation",
        );

        let mut missing_revoked_at = obj.clone();
        missing_revoked_at.remove("revoked_at");
        assert!(
            serde_json::from_value::<Revocation>(serde_json::Value::Object(missing_revoked_at))
                .is_err(),
            "Revocation wire form must reject a payload missing \
             'revoked_at' (no #[serde(default)] on the field) — a \
             relaxation would let undated tombstones slip into \
             revoked.jsonl and break operator-driven retention \
             purges that key on revoked_at < before_ms",
        );

        let payload_array_signature = serde_json::json!({
            "signature": vec![7u8; 64],
            "revoked_at": 1_700_000_000_000u64,
        });
        assert!(
            serde_json::from_value::<Revocation>(payload_array_signature).is_err(),
            "Revocation wire form must reject a JSON array of 64 \
             numbers in the signature slot — the custom sig_b58 \
             serializer is string-based; a relaxation would silently \
             accept a non-canonical wire form and split the \
             revoked.jsonl read path between string and array shapes",
        );

        let short_b58 = bs58::encode([0u8; 32]).into_string();
        let payload_short_b58 = serde_json::json!({
            "signature": short_b58,
            "revoked_at": 1_700_000_000_000u64,
        });
        assert!(
            serde_json::from_value::<Revocation>(payload_short_b58).is_err(),
            "Revocation wire form must reject a base58 string whose \
             decoded byte length is not 64 (e.g., a 32-byte pubkey \
             b58 placed in the signature slot); a relaxed length \
             check would zero-pad the [u8; 64] and silently make \
             every revocation's signature equality match return \
             false in is_revoked() — restoring live status to every \
             revoked capability",
        );
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
    fn scope_allows_tiers_pins_absent_non_array_allow_and_all_requested_must_match() {
        let empty = serde_json::json!({});
        let empty = empty.as_object().unwrap();
        assert!(
            scope_allows_tiers(empty, &["short"]),
            "absent 'tiers' field must allow any requested tiers; otherwise unscoped grants reject every tiered write",
        );
        assert!(
            scope_allows_tiers(empty, &[]),
            "absent 'tiers' field must allow an empty requested set; the absent-key branch is unconditional",
        );

        let non_array = serde_json::json!({ "tiers": null });
        let non_array = non_array.as_object().unwrap();
        assert!(
            scope_allows_tiers(non_array, &["short"]),
            "{{\"tiers\": null}} fails the as_array guard so the helper falls through to allow-all; otherwise null markers silently fail every tier check",
        );

        let bound = serde_json::json!({ "tiers": ["short", "long"] });
        let bound = bound.as_object().unwrap();
        assert!(
            scope_allows_tiers(bound, &["short"]),
            "scope tiers=[short,long] must allow requested [short]; a single-tier subset must pass",
        );
        assert!(
            scope_allows_tiers(bound, &["long"]),
            "scope tiers=[short,long] must allow requested [long]; per-tier subset must pass",
        );
        assert!(
            scope_allows_tiers(bound, &["short", "long"]),
            "scope tiers=[short,long] must allow the exact requested superset",
        );
        assert!(
            !scope_allows_tiers(bound, &["short", "sensitive"]),
            "scope tiers=[short,long] must NOT allow requested [short,sensitive] because every requested tier must be allowed; a regression that flipped all() to any() would silently leak writes across tiers",
        );

        let empty_array = serde_json::json!({ "tiers": [] });
        let empty_array = empty_array.as_object().unwrap();
        assert!(
            scope_allows_tiers(empty_array, &[]),
            "scope tiers=[] must allow an empty requested set; iter().all() over an empty requested set is vacuously true",
        );
        assert!(
            !scope_allows_tiers(empty_array, &["short"]),
            "scope tiers=[] must reject any non-empty requested set; the empty allowed array authorizes nothing",
        );

        let mixed_types = serde_json::json!({ "tiers": ["short", 42, "long"] });
        let mixed_types = mixed_types.as_object().unwrap();
        assert!(
            scope_allows_tiers(mixed_types, &["short", "long"]),
            "scope tiers=[short, 42, long] must drop the non-string entry via filter_map; requested [short, long] is fully covered by the remaining string entries",
        );
        assert!(
            !scope_allows_tiers(mixed_types, &["42"]),
            "scope tiers=[short, 42, long] must NOT allow requested [\"42\"]; non-string entries must be dropped before comparison so the integer 42 is not silently coerced to the string \"42\"",
        );
    }

    #[test]
    fn scope_allows_token_prefix_pins_absent_null_prefix_match_and_strict_none_path() {
        let empty = serde_json::json!({});
        let empty = empty.as_object().unwrap();
        assert!(
            scope_allows_token_prefix(empty, Some("anything")),
            "absent 'token_prefix' field must allow any Some(_); unscoped grants have no prefix gate",
        );
        assert!(
            scope_allows_token_prefix(empty, None),
            "absent 'token_prefix' field must allow actual=None; the absent-key branch is unconditional",
        );

        let explicit_null = serde_json::json!({ "token_prefix": null });
        let explicit_null = explicit_null.as_object().unwrap();
        assert!(
            scope_allows_token_prefix(explicit_null, Some("anything")),
            "{{\"token_prefix\": null}} is the documented unbounded marker and must allow any Some(_)",
        );
        assert!(
            scope_allows_token_prefix(explicit_null, None),
            "{{\"token_prefix\": null}} must allow actual=None; the null arm is unconditional",
        );

        let bound = serde_json::json!({ "token_prefix": "abc" });
        let bound = bound.as_object().unwrap();
        assert!(
            scope_allows_token_prefix(bound, Some("abcdef")),
            "scope token_prefix=\"abc\" must allow actual=Some(\"abcdef\"); the redaction-friendly contract is starts_with, not exact equality",
        );
        assert!(
            scope_allows_token_prefix(bound, Some("abc")),
            "scope token_prefix=\"abc\" must allow actual=Some(\"abc\"); the bare prefix is its own valid extension",
        );
        assert!(
            !scope_allows_token_prefix(bound, Some("ab")),
            "scope token_prefix=\"abc\" must NOT allow actual=Some(\"ab\"); shorter-than-prefix actuals must fail starts_with",
        );
        assert!(
            !scope_allows_token_prefix(bound, Some("xabc")),
            "scope token_prefix=\"abc\" must NOT allow actual=Some(\"xabc\"); the comparison is starts_with, not contains",
        );
        assert!(
            !scope_allows_token_prefix(bound, None),
            "scope token_prefix=\"abc\" must NOT allow actual=None; a regression that allowed None here would silently authorize unauthenticated callers under prefix-scoped peer grants",
        );

        let non_string = serde_json::json!({ "token_prefix": 42 });
        let non_string = non_string.as_object().unwrap();
        assert!(
            !scope_allows_token_prefix(non_string, Some("abc")),
            "a non-string 'token_prefix' must strict-deny Some(_); this is the one helper in the family whose malformed-scope path is deny, because tokens gate a security boundary",
        );
        assert!(
            !scope_allows_token_prefix(non_string, None),
            "a non-string 'token_prefix' must strict-deny None too; the malformed-scope deny path is unconditional on the prefix side",
        );
    }

    #[test]
    fn scope_allows_string_pins_absent_null_exact_match_and_non_string_path() {
        let empty = serde_json::json!({});
        let empty = empty.as_object().unwrap();
        assert!(
            scope_allows_string(empty, "x", Some("foo")),
            "absent field must allow any Some(_); otherwise unscoped grants reject every string-bearing request",
        );
        assert!(
            scope_allows_string(empty, "x", None),
            "absent field must allow actual=None; the absent-key branch must be unconditional, matching the rest of the scope_allows_* family",
        );

        let explicit_null = serde_json::json!({ "x": null });
        let explicit_null = explicit_null.as_object().unwrap();
        assert!(
            scope_allows_string(explicit_null, "x", Some("foo")),
            "{{\"x\": null}} is the documented unbounded marker and must allow any Some(_)",
        );
        assert!(
            scope_allows_string(explicit_null, "x", None),
            "{{\"x\": null}} must allow actual=None as well; the null arm is unconditional",
        );

        let bound = serde_json::json!({ "x": "foo" });
        let bound = bound.as_object().unwrap();
        assert!(
            scope_allows_string(bound, "x", Some("foo")),
            "scope {{\"x\": \"foo\"}} must allow actual=Some(\"foo\") on exact match",
        );
        assert!(
            !scope_allows_string(bound, "x", Some("foobar")),
            "scope {{\"x\": \"foo\"}} must NOT allow actual=Some(\"foobar\"); the equality is strict, not starts_with",
        );
        assert!(
            !scope_allows_string(bound, "x", Some("")),
            "scope {{\"x\": \"foo\"}} must NOT allow actual=Some(\"\"); empty does not equal a bound non-empty value",
        );
        assert!(
            !scope_allows_string(bound, "x", None),
            "scope {{\"x\": \"foo\"}} must NOT allow actual=None; Option<&str> equality means Some(_) and None never match",
        );

        let non_string = serde_json::json!({ "x": 42 });
        let non_string = non_string.as_object().unwrap();
        assert!(
            scope_allows_string(non_string, "x", None),
            "a non-string scope value compares via value.as_str()==None which equals actual=None; this is the documented None-vs-None equality edge",
        );
        assert!(
            !scope_allows_string(non_string, "x", Some("foo")),
            "a non-string scope value must reject any Some(_) since None != Some(_) under Option equality; a regression that returned true here would silently authorize every string under a malformed scope object",
        );
    }

    #[test]
    fn scope_allows_before_ms_pins_absent_null_present_compare_and_zero_fallback() {
        let empty = serde_json::json!({});
        let empty = empty.as_object().unwrap();
        assert!(
            scope_allows_before_ms(empty, 0),
            "absent 'before_ms' field must allow any threshold including zero; otherwise unscoped grants reject every purge",
        );
        assert!(
            scope_allows_before_ms(empty, u64::MAX),
            "absent 'before_ms' field must allow the maximum threshold; the absent-key branch is unconditional",
        );

        let explicit_null = serde_json::json!({ "before_ms": null });
        let explicit_null = explicit_null.as_object().unwrap();
        assert!(
            scope_allows_before_ms(explicit_null, 0),
            "{{\"before_ms\": null}} is the documented unbounded marker and must allow any threshold",
        );
        assert!(
            scope_allows_before_ms(explicit_null, u64::MAX),
            "{{\"before_ms\": null}} must allow the maximum threshold; a regression that flips this would silently reject the unbounded marker",
        );

        let bounded = serde_json::json!({ "before_ms": 100u64 });
        let bounded = bounded.as_object().unwrap();
        assert!(
            scope_allows_before_ms(bounded, 0),
            "scope before_ms=100 must allow request before_ms=0; the comparison is inclusive on the low end",
        );
        assert!(
            scope_allows_before_ms(bounded, 99),
            "scope before_ms=100 must allow request before_ms=99; below the scoped maximum",
        );
        assert!(
            scope_allows_before_ms(bounded, 100),
            "scope before_ms=100 must allow request before_ms=100; the <= comparison must include equality, otherwise boundary-bound purges silently fail",
        );
        assert!(
            !scope_allows_before_ms(bounded, 101),
            "scope before_ms=100 must NOT allow request before_ms=101; a regression that flipped <= to < or used the wrong direction would silently authorize wider purge windows",
        );

        let malformed = serde_json::json!({ "before_ms": "oops" });
        let malformed = malformed.as_object().unwrap();
        assert!(
            scope_allows_before_ms(malformed, 0),
            "a non-u64 'before_ms' must collapse the threshold to zero via unwrap_or(0); zero is still <= 0, so only the strict-zero request is allowed",
        );
        assert!(
            !scope_allows_before_ms(malformed, 1),
            "a non-u64 'before_ms' must reject any non-zero threshold via the zero-fallback; relaxing this to unwrap_or(u64::MAX) would silently allow any purge through a malformed scope object",
        );
    }

    #[test]
    fn scope_allows_record_id_pins_absent_key_exact_match_and_non_string_fallthrough() {
        let empty = serde_json::json!({});
        let empty = empty.as_object().unwrap();
        assert!(
            scope_allows_record_id(empty, "rec-1"),
            "absent 'record_id' field must default to allow; otherwise unscoped record-bearing memory ops are silently rejected",
        );
        assert!(
            scope_allows_record_id(empty, ""),
            "absent 'record_id' field must allow even an empty record_id so the unscoped default is unconditional, matching scope_allows_apply",
        );

        let bound = serde_json::json!({ "record_id": "rec-1" });
        let bound = bound.as_object().unwrap();
        assert!(
            scope_allows_record_id(bound, "rec-1"),
            "scope {{\"record_id\": \"rec-1\"}} must allow exactly \"rec-1\"; otherwise the equality check silently denies its own pinned record",
        );
        assert!(
            !scope_allows_record_id(bound, "rec-12"),
            "scope {{\"record_id\": \"rec-1\"}} must NOT allow \"rec-12\"; a regression that swapped equality for starts_with would silently broaden authority across record-id prefixes",
        );
        assert!(
            !scope_allows_record_id(bound, "rec-1a"),
            "scope {{\"record_id\": \"rec-1\"}} must NOT allow \"rec-1a\"; the equality check must not silently widen to contains/starts_with",
        );
        assert!(
            !scope_allows_record_id(bound, ""),
            "scope {{\"record_id\": \"rec-1\"}} must NOT allow the empty record_id; defaulting to allow on empty would silently bypass the bound scope",
        );

        let non_string = serde_json::json!({ "record_id": 42 });
        let non_string = non_string.as_object().unwrap();
        assert!(
            scope_allows_record_id(non_string, "rec-1"),
            "a non-string 'record_id' field must fall through to allow for a normal record_id; otherwise the helper would diverge from scope_allows_apply and reject partially-typed scope objects",
        );
        assert!(
            scope_allows_record_id(non_string, ""),
            "a non-string 'record_id' field must fall through to allow for an empty record_id as well, matching the rest of the scope_allows_* family",
        );
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
    fn scope_allows_optional_bool_pins_absent_null_bool_match_and_none_strict_deny() {
        let empty = serde_json::json!({});
        let empty = empty.as_object().unwrap();
        assert!(
            scope_allows_optional_bool(empty, "x", Some(true)),
            "absent field must allow actual=Some(true); the absent-key branch is unconditional and matches the rest of the scope_allows_optional_* family",
        );
        assert!(
            scope_allows_optional_bool(empty, "x", Some(false)),
            "absent field must allow actual=Some(false); the absent-key branch is unconditional and cannot bias toward true",
        );
        assert!(
            scope_allows_optional_bool(empty, "x", None),
            "absent field must allow actual=None; otherwise unscoped grants reject every request whose optional bool is omitted",
        );

        let explicit_null = serde_json::json!({ "x": null });
        let explicit_null = explicit_null.as_object().unwrap();
        assert!(
            scope_allows_optional_bool(explicit_null, "x", Some(true)),
            "{{\"x\": null}} is the documented unbounded marker and must allow actual=Some(true)",
        );
        assert!(
            scope_allows_optional_bool(explicit_null, "x", Some(false)),
            "{{\"x\": null}} must allow actual=Some(false) too; the null arm is unconditional on the bool side",
        );
        assert!(
            scope_allows_optional_bool(explicit_null, "x", None),
            "{{\"x\": null}} must allow actual=None as well; a regression that demanded actual=Some(_) under the null arm would silently reject the documented unbounded marker",
        );

        let bound_true = serde_json::json!({ "x": true });
        let bound_true = bound_true.as_object().unwrap();
        assert!(
            scope_allows_optional_bool(bound_true, "x", Some(true)),
            "scope {{\"x\": true}} must allow actual=Some(true); otherwise the equality check silently denies its own pinned value",
        );
        assert!(
            !scope_allows_optional_bool(bound_true, "x", Some(false)),
            "scope {{\"x\": true}} must NOT allow actual=Some(false); a regression that degraded the equality to one-way would silently authorize the opposite bool",
        );
        assert!(
            !scope_allows_optional_bool(bound_true, "x", None),
            "scope {{\"x\": true}} must NOT allow actual=None; the unwrap_or(false) branch is the strict-deny that distinguishes optional_bool from optional_string, and a regression to unwrap_or(true) would silently authorize unspecified bool requests through bool-bound scopes",
        );

        let bound_false = serde_json::json!({ "x": false });
        let bound_false = bound_false.as_object().unwrap();
        assert!(
            scope_allows_optional_bool(bound_false, "x", Some(false)),
            "scope {{\"x\": false}} must allow actual=Some(false); otherwise dry-run-only style bool gates never authorize their own pinned value",
        );
        assert!(
            !scope_allows_optional_bool(bound_false, "x", Some(true)),
            "scope {{\"x\": false}} must NOT allow actual=Some(true); a regression here would silently let a false-bound grant authorize the opposite bool",
        );
        assert!(
            !scope_allows_optional_bool(bound_false, "x", None),
            "scope {{\"x\": false}} must NOT allow actual=None; the strict-deny on actual=None is symmetric across both bound bools and protects boolean gates where 'not specified' is not the same as 'allowed'",
        );

        let non_bool = serde_json::json!({ "x": "oops" });
        let non_bool = non_bool.as_object().unwrap();
        assert!(
            !scope_allows_optional_bool(non_bool, "x", Some(true)),
            "a non-bool scope value compares via value.as_bool()==None which never equals Some(true), so the helper must strict-deny; a regression that treated malformed scope objects as allow-all would silently authorize bool-gated operations through string-typed scope fields",
        );
        assert!(
            !scope_allows_optional_bool(non_bool, "x", Some(false)),
            "a non-bool scope value must strict-deny actual=Some(false) too; value.as_bool() returns None which never equals Some(false), and the deny path is symmetric across both bools",
        );
        assert!(
            !scope_allows_optional_bool(non_bool, "x", None),
            "a non-bool scope value with actual=None must strict-deny via the .map(...).unwrap_or(false) branch; this is the path where actual=None never enters the closure and the unwrap_or(false) fires regardless of the scope value's type",
        );
    }

    #[test]
    fn scope_allows_optional_before_ms_pins_absent_null_compare_and_none_strict_deny() {
        let empty = serde_json::json!({});
        let empty = empty.as_object().unwrap();
        assert!(
            scope_allows_optional_before_ms(empty, Some(0)),
            "absent 'before_ms' field must allow actual=Some(0); the absent-key branch is unconditional",
        );
        assert!(
            scope_allows_optional_before_ms(empty, Some(u64::MAX)),
            "absent 'before_ms' field must allow actual=Some(u64::MAX); a regression that gated the absent path on the magnitude would silently reject the unbounded request through an unscoped grant",
        );
        assert!(
            scope_allows_optional_before_ms(empty, None),
            "absent 'before_ms' field must allow actual=None; otherwise unscoped grants reject every request whose before_ms is omitted",
        );

        let explicit_null = serde_json::json!({ "before_ms": null });
        let explicit_null = explicit_null.as_object().unwrap();
        assert!(
            scope_allows_optional_before_ms(explicit_null, Some(0)),
            "{{\"before_ms\": null}} is the documented unbounded marker and must allow actual=Some(0)",
        );
        assert!(
            scope_allows_optional_before_ms(explicit_null, Some(u64::MAX)),
            "{{\"before_ms\": null}} must allow actual=Some(u64::MAX); a regression that demanded a finite bound under the null arm would silently reject the unbounded marker's intended use",
        );
        assert!(
            scope_allows_optional_before_ms(explicit_null, None),
            "{{\"before_ms\": null}} must allow actual=None as well; the null arm is unconditional regardless of whether the request supplied a threshold",
        );

        let bounded = serde_json::json!({ "before_ms": 100u64 });
        let bounded = bounded.as_object().unwrap();
        assert!(
            scope_allows_optional_before_ms(bounded, Some(0)),
            "scope before_ms=100 must allow actual=Some(0); the comparison is inclusive on the low end",
        );
        assert!(
            scope_allows_optional_before_ms(bounded, Some(99)),
            "scope before_ms=100 must allow actual=Some(99); below the scoped maximum",
        );
        assert!(
            scope_allows_optional_before_ms(bounded, Some(100)),
            "scope before_ms=100 must allow actual=Some(100); the <= comparison must include equality, otherwise boundary-bound purges silently fail (matches scope_allows_before_ms on the strict side)",
        );
        assert!(
            !scope_allows_optional_before_ms(bounded, Some(101)),
            "scope before_ms=100 must NOT allow actual=Some(101); a regression that flipped <= to >= or used the wrong direction would silently authorize wider purge windows on the optional side",
        );
        assert!(
            !scope_allows_optional_before_ms(bounded, None),
            "scope before_ms=100 must NOT allow actual=None; the .map(...).unwrap_or(false) strict-deny on actual=None is what distinguishes optional_before_ms from optional_string, and dropping it would silently authorize unspecified-before_ms purges under bound scopes",
        );

        let malformed = serde_json::json!({ "before_ms": "oops" });
        let malformed = malformed.as_object().unwrap();
        assert!(
            scope_allows_optional_before_ms(malformed, Some(0)),
            "a non-u64 'before_ms' must collapse the threshold to zero via unwrap_or(0); zero is still <= 0 so only the strict-zero request is allowed",
        );
        assert!(
            !scope_allows_optional_before_ms(malformed, Some(1)),
            "a non-u64 'before_ms' must reject actual=Some(1) via the zero-fallback; relaxing this to unwrap_or(u64::MAX) would silently authorize any purge through a malformed scope object",
        );
        assert!(
            !scope_allows_optional_before_ms(malformed, None),
            "a non-u64 'before_ms' with actual=None must strict-deny via .map(...).unwrap_or(false); this is the path where the zero-fallback never fires because actual=None never enters the closure, so the helper falls through to the outer unwrap_or(false) strict-deny",
        );
    }

    #[test]
    fn scope_allows_optional_limit_pins_absent_none_allow_and_present_compare() {
        let empty = serde_json::json!({});
        let empty = empty.as_object().unwrap();
        assert!(
            scope_allows_optional_limit(empty, None),
            "absent 'limit' field must allow actual=None; the absent-key branch is unconditional and matches the rest of the scope_allows_* family",
        );
        assert!(
            scope_allows_optional_limit(empty, Some(0)),
            "absent 'limit' field must allow actual=Some(0); the absent-key branch cannot gate on the magnitude",
        );
        assert!(
            scope_allows_optional_limit(empty, Some(usize::MAX)),
            "absent 'limit' field must allow actual=Some(usize::MAX); otherwise unscoped grants silently reject large-limit requests through an absent gate",
        );

        let bounded = serde_json::json!({ "limit": 10u64 });
        let bounded = bounded.as_object().unwrap();
        assert!(
            scope_allows_optional_limit(bounded, None),
            "scope {{\"limit\": 10}} must allow actual=None; the .map(...).unwrap_or(true) branch is the documented divergence from optional_bool/optional_before_ms — limit treats 'not specified' as allowed under a bound, and a regression to unwrap_or(false) would silently reject unspecified-limit requests",
        );
        assert!(
            scope_allows_optional_limit(bounded, Some(0)),
            "scope {{\"limit\": 10}} must allow actual=Some(0); the comparison is inclusive on the low end",
        );
        assert!(
            scope_allows_optional_limit(bounded, Some(9)),
            "scope {{\"limit\": 10}} must allow actual=Some(9); below the scoped maximum",
        );
        assert!(
            scope_allows_optional_limit(bounded, Some(10)),
            "scope {{\"limit\": 10}} must allow actual=Some(10); the <= comparison must include equality, otherwise boundary-bound limits silently fail",
        );
        assert!(
            !scope_allows_optional_limit(bounded, Some(11)),
            "scope {{\"limit\": 10}} must NOT allow actual=Some(11); a regression that flipped <= to >= or used the wrong direction would silently authorize wider limits",
        );

        let malformed = serde_json::json!({ "limit": "oops" });
        let malformed = malformed.as_object().unwrap();
        assert!(
            scope_allows_optional_limit(malformed, None),
            "a non-u64 'limit' with actual=None must allow via .map(...).unwrap_or(true); the malformed-scope path is documented to allow on actual=None so unspecified-limit reads keep working even through partially-typed scope objects",
        );
        assert!(
            scope_allows_optional_limit(malformed, Some(0)),
            "a non-u64 'limit' must collapse the threshold to zero via unwrap_or(0); zero is still <= 0 so only the strict-zero request is allowed on the present-with-Some side",
        );
        assert!(
            !scope_allows_optional_limit(malformed, Some(1)),
            "a non-u64 'limit' must reject actual=Some(1) via the zero-fallback; relaxing this to unwrap_or(u64::MAX) would silently authorize arbitrarily large limits through a malformed scope object",
        );
    }

    #[test]
    fn scope_allows_optional_string_pins_absent_null_present_match_and_none_allow_through() {
        let empty = serde_json::json!({});
        let empty = empty.as_object().unwrap();
        assert!(
            scope_allows_optional_string(empty, "x", Some("foo")),
            "absent field must allow actual=Some(_); the absent-key branch is unconditional",
        );
        assert!(
            scope_allows_optional_string(empty, "x", Some("")),
            "absent field must allow actual=Some(\"\"); the absent-key branch cannot gate on string content",
        );
        assert!(
            scope_allows_optional_string(empty, "x", None),
            "absent field must allow actual=None; otherwise unscoped grants reject every request whose optional string is omitted",
        );

        let explicit_null = serde_json::json!({ "x": null });
        let explicit_null = explicit_null.as_object().unwrap();
        assert!(
            scope_allows_optional_string(explicit_null, "x", Some("foo")),
            "{{\"x\": null}} is the documented unbounded marker and must allow actual=Some(_)",
        );
        assert!(
            scope_allows_optional_string(explicit_null, "x", None),
            "{{\"x\": null}} must allow actual=None as well; the null arm is unconditional",
        );

        let bound = serde_json::json!({ "x": "foo" });
        let bound = bound.as_object().unwrap();
        assert!(
            scope_allows_optional_string(bound, "x", None),
            "scope {{\"x\": \"foo\"}} must allow actual=None; the .map(...).unwrap_or(true) branch is the documented divergence from optional_bool/optional_before_ms — optional_string treats 'not specified' as allowed under a bound, and a regression to unwrap_or(false) would silently reject unspecified-string requests through bound scopes",
        );
        assert!(
            scope_allows_optional_string(bound, "x", Some("foo")),
            "scope {{\"x\": \"foo\"}} must allow actual=Some(\"foo\") on exact match; otherwise the equality check silently denies its own pinned value",
        );
        assert!(
            !scope_allows_optional_string(bound, "x", Some("bar")),
            "scope {{\"x\": \"foo\"}} must NOT allow actual=Some(\"bar\"); the equality is strict, otherwise bound string scopes silently authorize unrelated requests",
        );
        assert!(
            !scope_allows_optional_string(bound, "x", Some("foobar")),
            "scope {{\"x\": \"foo\"}} must NOT allow actual=Some(\"foobar\"); a regression that swapped equality for starts_with would silently widen string authority across prefixes",
        );
        assert!(
            !scope_allows_optional_string(bound, "x", Some("")),
            "scope {{\"x\": \"foo\"}} must NOT allow actual=Some(\"\"); the empty string does not equal a bound non-empty value",
        );

        let non_string = serde_json::json!({ "x": 42 });
        let non_string = non_string.as_object().unwrap();
        assert!(
            scope_allows_optional_string(non_string, "x", None),
            "a non-string scope value with actual=None must allow via .map(...).unwrap_or(true); the malformed-scope path on actual=None never enters the equality closure, so the unwrap_or(true) fires and produces the documented allow-through",
        );
        assert!(
            !scope_allows_optional_string(non_string, "x", Some("foo")),
            "a non-string scope value with actual=Some(_) must reject; value.as_str() returns None which never equals Some(\"foo\"), and a regression that treated malformed scope objects as allow-all on the Some side would silently authorize every string-bearing request",
        );
        assert!(
            !scope_allows_optional_string(non_string, "x", Some("")),
            "a non-string scope value must also reject actual=Some(\"\"); the equality is None != Some(\"\") symmetrically across all Some(_) inputs",
        );
    }

    #[test]
    fn scope_allows_duplicate_risk_pins_absent_null_canonicalization_and_strict_none_path() {
        let empty = serde_json::json!({});
        let empty = empty.as_object().unwrap();
        assert!(
            scope_allows_duplicate_risk(empty, Some("at-least-once")),
            "absent 'duplicate_risk' field must allow any Some(_); unscoped grants have no duplicate-risk gate",
        );
        assert!(
            scope_allows_duplicate_risk(empty, None),
            "absent 'duplicate_risk' field must allow actual=None; the absent-key branch is unconditional",
        );

        let explicit_null = serde_json::json!({ "duplicate_risk": null });
        let explicit_null = explicit_null.as_object().unwrap();
        assert!(
            scope_allows_duplicate_risk(explicit_null, Some("at-least-once")),
            "{{\"duplicate_risk\": null}} is the documented unbounded marker and must allow any Some(_)",
        );
        assert!(
            scope_allows_duplicate_risk(explicit_null, None),
            "{{\"duplicate_risk\": null}} must allow actual=None; the null arm is unconditional and short-circuits before the let-else strict-deny",
        );

        let hyphen = serde_json::json!({ "duplicate_risk": "at-least-once" });
        let hyphen = hyphen.as_object().unwrap();
        assert!(
            scope_allows_duplicate_risk(hyphen, Some("at-least-once")),
            "scope duplicate_risk=\"at-least-once\" must allow actual=Some(\"at-least-once\"); otherwise the canonicalization silently rejects its own pinned value",
        );
        assert!(
            scope_allows_duplicate_risk(hyphen, Some("at_least_once")),
            "scope duplicate_risk=\"at-least-once\" must allow actual=Some(\"at_least_once\"); underscore and hyphen are documented interchangeable forms, so a regression that dropped .replace('_','-') would silently reject operator-supplied underscore forms against wire hyphen forms",
        );
        assert!(
            !scope_allows_duplicate_risk(hyphen, Some("idempotent")),
            "scope duplicate_risk=\"at-least-once\" must NOT allow actual=Some(\"idempotent\"); the equality is strict (post-canonicalization) and a regression here would silently authorize unrelated risk classes",
        );
        assert!(
            !scope_allows_duplicate_risk(hyphen, None),
            "scope duplicate_risk=\"at-least-once\" must NOT allow actual=None; the let-else strict-deny on actual=None protects the duplicate-risk gate from unspecified-risk requests, and flipping it to allow would leak through the a2a duplicate gate",
        );

        let underscore = serde_json::json!({ "duplicate_risk": "at_least_once" });
        let underscore = underscore.as_object().unwrap();
        assert!(
            scope_allows_duplicate_risk(underscore, Some("at-least-once")),
            "scope duplicate_risk=\"at_least_once\" must allow actual=Some(\"at-least-once\"); the canonicalization is symmetric across both sides of the comparison, so an operator-supplied underscore form authorizes a wire hyphen form",
        );
        assert!(
            scope_allows_duplicate_risk(underscore, Some("at_least_once")),
            "scope duplicate_risk=\"at_least_once\" must allow actual=Some(\"at_least_once\"); the canonicalized form matches itself",
        );

        let non_string = serde_json::json!({ "duplicate_risk": 42 });
        let non_string = non_string.as_object().unwrap();
        assert!(
            !scope_allows_duplicate_risk(non_string, Some("at-least-once")),
            "a non-string 'duplicate_risk' must strict-deny Some(_); value.as_str() returns None and the .unwrap_or(false) fires, so a regression that allowed Some(_) through a malformed scope would silently authorize any duplicate_risk",
        );
        assert!(
            !scope_allows_duplicate_risk(non_string, None),
            "a non-string 'duplicate_risk' with actual=None must strict-deny via the let-else; the let-else fires before the as_str() check, so actual=None denies regardless of the scope value's type",
        );
    }

    #[test]
    fn optional_pubkey_b58_or_null_pins_absent_null_non_string_decode_error_and_length() {
        let empty = serde_json::json!({});
        let empty = empty.as_object().unwrap();
        assert!(
            optional_pubkey_b58_or_null("peers.revoke", empty, "k").is_ok(),
            "absent field must be Ok; the let-else short-circuits before any decoding so unscoped grants do not fail at the pubkey gate",
        );

        let explicit_null = serde_json::json!({ "k": null });
        let explicit_null = explicit_null.as_object().unwrap();
        assert!(
            optional_pubkey_b58_or_null("peers.revoke", explicit_null, "k").is_ok(),
            "{{\"k\": null}} must be Ok; the null arm is the documented unbounded marker and must short-circuit before the as_str() and decode checks",
        );

        let valid_key = bs58::encode([0u8; 32]).into_string();
        let valid = serde_json::json!({ "k": valid_key });
        let valid = valid.as_object().unwrap();
        assert!(
            optional_pubkey_b58_or_null("peers.revoke", valid, "k").is_ok(),
            "a base58-encoded 32-byte key must be Ok; constructed from a 32-byte zero array to avoid coupling the test to a magic string while still exercising the length-check success path",
        );

        let non_string = serde_json::json!({ "k": 42 });
        let non_string = non_string.as_object().unwrap();
        let err = optional_pubkey_b58_or_null("peers.revoke", non_string, "k").unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("base58 public key or null")),
            "non-string scope value must produce the 'base58 public key or null' error path; got {err:?}. A regression that accepted non-string scope values would silently authorize partially-typed scope objects through the pubkey gate.",
        );

        let invalid_alphabet = serde_json::json!({ "k": "0OIl" });
        let invalid_alphabet = invalid_alphabet.as_object().unwrap();
        let err = optional_pubkey_b58_or_null("peers.revoke", invalid_alphabet, "k").unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("base58 public key or null")),
            "non-base58 string must produce the same 'base58 public key or null' error via bs58::decode failure; got {err:?}. A regression that swallowed bs58::decode errors as Ok would silently accept characters outside the documented base58 alphabet.",
        );

        let short_key = bs58::encode([0u8; 31]).into_string();
        let short = serde_json::json!({ "k": short_key });
        let short = short.as_object().unwrap();
        let err = optional_pubkey_b58_or_null("peers.revoke", short, "k").unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("32-byte public key")),
            "a base58 string that decodes to 31 bytes must produce the '32-byte public key' error path (distinct from the 'base58 public key or null' decode-failure path); got {err:?}. A regression that dropped the length check would silently accept shorter base58 strings as ed25519 public keys.",
        );

        let long_key = bs58::encode([0u8; 33]).into_string();
        let long = serde_json::json!({ "k": long_key });
        let long = long.as_object().unwrap();
        let err = optional_pubkey_b58_or_null("peers.revoke", long, "k").unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("32-byte public key")),
            "a base58 string that decodes to 33 bytes must produce the '32-byte public key' error path symmetrically with the short-key case; got {err:?}. A regression that dropped the length check would silently accept longer base58 strings as ed25519 public keys.",
        );
    }

    #[test]
    fn optional_non_empty_string_or_null_pins_absent_null_non_string_and_empty_rejection() {
        let empty = serde_json::json!({});
        let empty = empty.as_object().unwrap();
        assert!(
            optional_non_empty_string_or_null("chain.flush", empty, "x").is_ok(),
            "absent field must be Ok; the let-else short-circuits before the type and emptiness checks so unscoped grants do not fail at the non-empty-string gate",
        );

        let explicit_null = serde_json::json!({ "x": null });
        let explicit_null = explicit_null.as_object().unwrap();
        assert!(
            optional_non_empty_string_or_null("chain.flush", explicit_null, "x").is_ok(),
            "{{\"x\": null}} must be Ok; the null arm is the documented unbounded marker and must short-circuit before the is_empty check",
        );

        let valid = serde_json::json!({ "x": "abc" });
        let valid = valid.as_object().unwrap();
        assert!(
            optional_non_empty_string_or_null("chain.flush", valid, "x").is_ok(),
            "a non-empty string must be Ok; otherwise the gate silently rejects its own intended input",
        );

        let non_string = serde_json::json!({ "x": 42 });
        let non_string = non_string.as_object().unwrap();
        let err = optional_non_empty_string_or_null("chain.flush", non_string, "x").unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("non-empty string or null")),
            "non-string scope value must produce the 'non-empty string or null' error; got {err:?}. A regression that accepted non-string values would silently authorize partially-typed scope objects through the chain identifier gate.",
        );

        let empty_str = serde_json::json!({ "x": "" });
        let empty_str = empty_str.as_object().unwrap();
        let err = optional_non_empty_string_or_null("chain.flush", empty_str, "x").unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("non-empty string or null")),
            "empty string must produce the same 'non-empty string or null' error; got {err:?}. A regression that accepted \"\" would silently authorize zero-length identifiers under chain.batch_id, mint, and cluster fields.",
        );
    }

    #[test]
    fn optional_base58_prefix_or_null_pins_absent_null_non_string_empty_and_decode_error() {
        let empty = serde_json::json!({});
        let empty = empty.as_object().unwrap();
        assert!(
            optional_base58_prefix_or_null("peers.revoke", empty, "k").is_ok(),
            "absent field must be Ok; the let-else short-circuits before any type or alphabet checks",
        );

        let explicit_null = serde_json::json!({ "k": null });
        let explicit_null = explicit_null.as_object().unwrap();
        assert!(
            optional_base58_prefix_or_null("peers.revoke", explicit_null, "k").is_ok(),
            "{{\"k\": null}} must be Ok; the null arm is the documented unbounded marker for the token-prefix gate",
        );

        let valid = serde_json::json!({ "k": "abc" });
        let valid = valid.as_object().unwrap();
        assert!(
            optional_base58_prefix_or_null("peers.revoke", valid, "k").is_ok(),
            "a non-empty decodable base58 prefix like \"abc\" must be Ok; otherwise the gate silently rejects its own intended input",
        );

        let non_string = serde_json::json!({ "k": 42 });
        let non_string = non_string.as_object().unwrap();
        let err = optional_base58_prefix_or_null("peers.revoke", non_string, "k").unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("non-empty base58 prefix or null")),
            "non-string scope value must produce the 'non-empty base58 prefix or null' error; got {err:?}. A regression that accepted non-string values would silently authorize partially-typed scope objects through the token-prefix gate.",
        );

        let empty_str = serde_json::json!({ "k": "" });
        let empty_str = empty_str.as_object().unwrap();
        let err = optional_base58_prefix_or_null("peers.revoke", empty_str, "k").unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("non-empty base58 prefix or null")),
            "empty token_prefix must produce the same error; got {err:?}. A regression that accepted \"\" would collapse the prefix-allows-all into the gate, silently authorizing any token through a prefix-scoped peer grant.",
        );

        let bad_alphabet = serde_json::json!({ "k": "0OIl" });
        let bad_alphabet = bad_alphabet.as_object().unwrap();
        let err = optional_base58_prefix_or_null("peers.revoke", bad_alphabet, "k").unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("non-empty base58 prefix or null")),
            "a non-base58 string like \"0OIl\" (contains characters outside the base58 alphabet) must produce the same error via bs58::decode failure; got {err:?}. A regression that swallowed decode errors would silently widen the token_prefix gate beyond the documented base58 alphabet.",
        );
    }

    #[test]
    fn optional_positive_integer_pins_absent_positive_zero_negative_and_non_integer() {
        let empty = serde_json::json!({});
        let empty = empty.as_object().unwrap();
        assert!(
            optional_positive_integer("audit.verify", empty, "x").is_ok(),
            "absent field must be Ok; the if-let short-circuits before the > 0 check",
        );

        let one = serde_json::json!({ "x": 1u64 });
        let one = one.as_object().unwrap();
        assert!(
            optional_positive_integer("audit.verify", one, "x").is_ok(),
            "{{\"x\": 1}} must be Ok; the > 0 boundary must include 1, otherwise the smallest valid window/limit is silently rejected",
        );

        let max = serde_json::json!({ "x": u64::MAX });
        let max = max.as_object().unwrap();
        assert!(
            optional_positive_integer("audit.verify", max, "x").is_ok(),
            "{{\"x\": u64::MAX}} must be Ok; the helper must not gate the upper end of the u64 range, since callers depend on as_u64 round-tripping the full domain",
        );

        let zero = serde_json::json!({ "x": 0u64 });
        let zero = zero.as_object().unwrap();
        let err = optional_positive_integer("audit.verify", zero, "x").unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("positive integer")),
            "{{\"x\": 0}} must produce the 'positive integer' error; got {err:?}. A regression that flipped > 0 to >= 0 would silently accept zero limits and zero windows, producing always-empty audit and chain queries through a permissive grant.",
        );

        let negative = serde_json::json!({ "x": -1i64 });
        let negative = negative.as_object().unwrap();
        let err = optional_positive_integer("audit.verify", negative, "x").unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("positive integer")),
            "{{\"x\": -1}} must produce the 'positive integer' error via the value.as_u64() returning None on a negative number; got {err:?}. A regression that fell back to as_i64 would silently accept negative integers.",
        );

        let non_int = serde_json::json!({ "x": "oops" });
        let non_int = non_int.as_object().unwrap();
        let err = optional_positive_integer("audit.verify", non_int, "x").unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("positive integer")),
            "non-integer scope value must produce the 'positive integer' error; got {err:?}. A regression that accepted non-integer values would silently let partially-typed scope objects through the positive-integer gate.",
        );

        let null_value = serde_json::json!({ "x": null });
        let null_value = null_value.as_object().unwrap();
        let err = optional_positive_integer("audit.verify", null_value, "x").unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("positive integer")),
            "{{\"x\": null}} must produce the 'positive integer' error; got {err:?}. Unlike optional_non_negative_integer_or_null, this helper has no or_null variant — null is not the documented unbounded marker for limit/window, and a regression that added a null bypass would silently authorize unbounded chain/audit queries through a null-valued scope field.",
        );
    }

    #[test]
    fn optional_string_enum_or_null_pins_absent_null_allowed_unsupported_and_non_string() {
        let allowed = &["compute", "memory", "tool", "message", "registration"];

        let empty = serde_json::json!({});
        let empty = empty.as_object().unwrap();
        assert!(
            optional_string_enum_or_null("chain.flush", empty, "r", allowed).is_ok(),
            "absent field must be Ok; the let-else short-circuits before the null, type, and enum checks so unscoped grants do not fail at the resource-enum gate",
        );

        let explicit_null = serde_json::json!({ "r": null });
        let explicit_null = explicit_null.as_object().unwrap();
        assert!(
            optional_string_enum_or_null("chain.flush", explicit_null, "r", allowed).is_ok(),
            "{{\"r\": null}} must be Ok; null is the documented unbounded-resource marker and must short-circuit before the as_str type check, otherwise unbounded chain.resource grants stop authorizing themselves",
        );

        let allowed_value = serde_json::json!({ "r": "compute" });
        let allowed_value = allowed_value.as_object().unwrap();
        assert!(
            optional_string_enum_or_null("chain.flush", allowed_value, "r", allowed).is_ok(),
            "{{\"r\": \"compute\"}} must be Ok; an in-list resource string is the gate's intended input and a regression that flipped the contains check would silently reject every documented resource class",
        );

        let unsupported = serde_json::json!({ "r": "unknown" });
        let unsupported = unsupported.as_object().unwrap();
        let err =
            optional_string_enum_or_null("chain.flush", unsupported, "r", allowed).unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("unsupported value")),
            "{{\"r\": \"unknown\"}} must produce the 'unsupported value' error; got {err:?}. A regression that broadened the allowed list to include arbitrary strings would silently authorize unsupported resource classes through chain.resource.",
        );

        let non_string = serde_json::json!({ "r": 42 });
        let non_string = non_string.as_object().unwrap();
        let err =
            optional_string_enum_or_null("chain.flush", non_string, "r", allowed).unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("string or null")),
            "{{\"r\": 42}} must produce the 'string or null' error via as_str() returning None; got {err:?}. This is a distinct error path from the enum-miss arm — a regression that fell through to the contains check on a non-string would either panic or silently bypass the enum gate for partially-typed scope objects.",
        );
    }

    #[test]
    fn optional_string_or_null_pins_absent_null_string_and_non_string_rejection() {
        let empty = serde_json::json!({});
        let empty = empty.as_object().unwrap();
        assert!(
            optional_string_or_null("a2a.deliver", empty, "x").is_ok(),
            "absent field must be Ok; the if-let short-circuits before the !is_null && !is_string check so unscoped grants do not fail at the string-or-null gate",
        );

        let explicit_null = serde_json::json!({ "x": null });
        let explicit_null = explicit_null.as_object().unwrap();
        assert!(
            optional_string_or_null("a2a.deliver", explicit_null, "x").is_ok(),
            "{{\"x\": null}} must be Ok; the !is_null half of the conjunction short-circuits before the type check, since null is the documented unbounded marker for peer/task/lease/record_id/tool fields",
        );

        let string_value = serde_json::json!({ "x": "abc" });
        let string_value = string_value.as_object().unwrap();
        assert!(
            optional_string_or_null("a2a.deliver", string_value, "x").is_ok(),
            "any string must be Ok; this helper has no emptiness or enum check, so a regression that added one would silently shrink the accepted set below what callers expect",
        );

        let non_string = serde_json::json!({ "x": 42 });
        let non_string = non_string.as_object().unwrap();
        let err = optional_string_or_null("a2a.deliver", non_string, "x").unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("string or null")),
            "{{\"x\": 42}} must produce the 'string or null' error; got {err:?}. A regression that flipped the && polarity (e.g., to ||) would silently authorize numbers, arrays, and objects through identifier fields like a2a.task_id, peer_pubkey_b58, lease_id, memory.record_id, and tool.",
        );
    }

    #[test]
    fn optional_bool_pins_absent_bool_non_bool_and_null_rejection() {
        let empty = serde_json::json!({});
        let empty = empty.as_object().unwrap();
        assert!(
            optional_bool("memory.apply", empty, "x").is_ok(),
            "absent field must be Ok; the if-let short-circuits before is_boolean so unscoped grants do not fail at the strict-bool gate",
        );

        let true_value = serde_json::json!({ "x": true });
        let true_value = true_value.as_object().unwrap();
        assert!(
            optional_bool("memory.apply", true_value, "x").is_ok(),
            "{{\"x\": true}} must be Ok; otherwise scope-restricted memory.apply/audit.include_integrity grants can never bind the gate to the affirmative case",
        );

        let false_value = serde_json::json!({ "x": false });
        let false_value = false_value.as_object().unwrap();
        assert!(
            optional_bool("memory.apply", false_value, "x").is_ok(),
            "{{\"x\": false}} must be Ok; a regression that only accepted true would silently invert the gate and block apply=false / include_integrity=false grants from ever validating",
        );

        let string_value = serde_json::json!({ "x": "true" });
        let string_value = string_value.as_object().unwrap();
        let err = optional_bool("memory.apply", string_value, "x").unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("must be a boolean")),
            "{{\"x\": \"true\"}} must produce the 'must be a boolean' error; got {err:?}. A regression that fell through to a truthy-string parse would silently authorize partially-typed scopes where downstream as_bool() reads None and treats the field as missing — exactly the silent-default-false hazard the gate exists to prevent.",
        );

        let null_value = serde_json::json!({ "x": null });
        let null_value = null_value.as_object().unwrap();
        let err = optional_bool("memory.apply", null_value, "x").unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("must be a boolean")),
            "{{\"x\": null}} must produce the 'must be a boolean' error; got {err:?}. This is the strict-deny path that distinguishes optional_bool from optional_bool_or_null — memory.apply and audit.include_integrity have no documented unbounded-bool marker, and a regression that accepted null would silently merge the two helpers' contracts and start treating null as 'unset' across the audit/memory surface.",
        );
    }

    #[test]
    fn optional_bool_or_null_pins_absent_null_bool_and_non_bool_rejection() {
        let empty = serde_json::json!({});
        let empty = empty.as_object().unwrap();
        assert!(
            optional_bool_or_null("peers.revoke", empty, "x").is_ok(),
            "absent field must be Ok; the if-let short-circuits before the !is_null && !is_boolean check so unscoped peer grants do not fail at the bool-or-null gate",
        );

        let explicit_null = serde_json::json!({ "x": null });
        let explicit_null = explicit_null.as_object().unwrap();
        assert!(
            optional_bool_or_null("peers.revoke", explicit_null, "x").is_ok(),
            "{{\"x\": null}} must be Ok; the !is_null half of the conjunction short-circuits before is_boolean, and null is the documented unbounded marker for peers.self/peers.force",
        );

        let true_value = serde_json::json!({ "x": true });
        let true_value = true_value.as_object().unwrap();
        assert!(
            optional_bool_or_null("peers.revoke", true_value, "x").is_ok(),
            "{{\"x\": true}} must be Ok; otherwise a scope-restricted peers.revoke grant binding self=true can never validate against itself",
        );

        let false_value = serde_json::json!({ "x": false });
        let false_value = false_value.as_object().unwrap();
        assert!(
            optional_bool_or_null("peers.revoke", false_value, "x").is_ok(),
            "{{\"x\": false}} must be Ok; a regression that accepted only true would silently invert the gate and break force=false / self=false peer grants",
        );

        let string_value = serde_json::json!({ "x": "true" });
        let string_value = string_value.as_object().unwrap();
        let err = optional_bool_or_null("peers.revoke", string_value, "x").unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("boolean or null")),
            "{{\"x\": \"true\"}} must produce the 'boolean or null' error; got {err:?}. A regression that accepted truthy-looking strings or numbers would silently widen the gate beyond what peers.revoke/peers.unrevoke expects, and downstream as_bool() would still read None — letting the partially-typed scope bypass the bound entirely.",
        );
    }

    #[test]
    fn optional_non_negative_integer_or_null_pins_absent_null_zero_positive_negative_and_non_integer(
    ) {
        let empty = serde_json::json!({});
        let empty = empty.as_object().unwrap();
        assert!(
            optional_non_negative_integer_or_null("audit.verify", empty, "x").is_ok(),
            "absent field must be Ok; the if-let short-circuits before the !is_null && as_u64().is_none() check so unscoped audit/peer/chain queries do not fail at the non-negative-integer-or-null gate",
        );

        let explicit_null = serde_json::json!({ "x": null });
        let explicit_null = explicit_null.as_object().unwrap();
        assert!(
            optional_non_negative_integer_or_null("audit.verify", explicit_null, "x").is_ok(),
            "{{\"x\": null}} must be Ok; the !is_null half of the conjunction short-circuits before as_u64, and null is the documented unbounded marker for audit/peer/chain before_ms",
        );

        let zero = serde_json::json!({ "x": 0u64 });
        let zero = zero.as_object().unwrap();
        assert!(
            optional_non_negative_integer_or_null("audit.verify", zero, "x").is_ok(),
            "{{\"x\": 0}} must be Ok; this is the documented non-negative contract, not positive — a regression that copied the > 0 check from optional_positive_integer would silently reject before_ms=0 which the audit/chain readers treat as a valid lower-bound or unbounded equivalent",
        );

        let positive = serde_json::json!({ "x": 1u64 });
        let positive = positive.as_object().unwrap();
        assert!(
            optional_non_negative_integer_or_null("audit.verify", positive, "x").is_ok(),
            "{{\"x\": 1}} must be Ok; any positive u64 is valid",
        );

        let negative = serde_json::json!({ "x": -1i64 });
        let negative = negative.as_object().unwrap();
        let err = optional_non_negative_integer_or_null("audit.verify", negative, "x").unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("non-negative integer or null")),
            "{{\"x\": -1}} must produce the 'non-negative integer or null' error via as_u64() returning None on a negative number; got {err:?}. A regression that fell back to as_i64 would silently authorize backward time windows that audit/chain readers either treat as unbounded or as wrap-around large positive values.",
        );

        let non_int = serde_json::json!({ "x": "oops" });
        let non_int = non_int.as_object().unwrap();
        let err = optional_non_negative_integer_or_null("audit.verify", non_int, "x").unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("non-negative integer or null")),
            "non-integer string must produce the same 'non-negative integer or null' error; got {err:?}. A regression that accepted non-integer values would silently let partially-typed scope objects through the before_ms gate.",
        );
    }

    #[test]
    fn optional_string_array_pins_absent_array_non_array_non_string_entry_and_unsupported_entry() {
        let allowed = &["working", "episodic", "longterm"];

        let empty = serde_json::json!({});
        let empty = empty.as_object().unwrap();
        assert!(
            optional_string_array("memory.read", empty, "x", allowed).is_ok(),
            "absent field must be Ok; the let-else short-circuits before any type or contents check so unscoped memory grants do not fail at the tiers gate",
        );

        let empty_array = serde_json::json!({ "x": [] });
        let empty_array = empty_array.as_object().unwrap();
        assert!(
            optional_string_array("memory.read", empty_array, "x", allowed).is_ok(),
            "{{\"x\": []}} must be Ok; the for-loop trivially succeeds, and a regression that required a non-empty array would silently reject documented zero-tier scopes",
        );

        let valid = serde_json::json!({ "x": ["working", "longterm"] });
        let valid = valid.as_object().unwrap();
        assert!(
            optional_string_array("memory.read", valid, "x", allowed).is_ok(),
            "{{\"x\": [\"working\", \"longterm\"]}} must be Ok against the memory tier allowlist; otherwise the gate silently rejects its own intended input",
        );

        let non_array = serde_json::json!({ "x": {} });
        let non_array = non_array.as_object().unwrap();
        let err = optional_string_array("memory.read", non_array, "x", allowed).unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("must be an array")),
            "{{\"x\": {{}}}} must produce the 'must be an array' error via as_array() returning None on an object; got {err:?}. A regression that fell through to accept objects would silently widen memory.tiers to arbitrary keyed shapes and bypass the tier allow-list entirely.",
        );

        let non_string_entry = serde_json::json!({ "x": [42] });
        let non_string_entry = non_string_entry.as_object().unwrap();
        let err = optional_string_array("memory.read", non_string_entry, "x", allowed).unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("entries must be strings")),
            "{{\"x\": [42]}} must produce the 'entries must be strings' error via the inner as_str returning None; got {err:?}. A regression that skipped non-string entries would silently let partially-typed memory grants through with mixed-type tier arrays that downstream as_str() reads as missing.",
        );

        let unsupported = serde_json::json!({ "x": ["working", "unknown"] });
        let unsupported = unsupported.as_object().unwrap();
        let err = optional_string_array("memory.read", unsupported, "x", allowed).unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("unsupported value")),
            "{{\"x\": [\"working\", \"unknown\"]}} must produce the 'unsupported value' error on the second entry; got {err:?}. A regression that stopped iterating after the first entry would silently authorize unsupported tiers placed later in the list, breaking the tier allow-list for any grant binding ['working', 'X'] where X is outside the allowed set.",
        );
    }

    #[test]
    fn optional_string_enum_pins_absent_allowed_unsupported_non_string_and_null() {
        let allowed = &["idempotent", "operator-accepted", "operator_accepted"];

        let empty = serde_json::json!({});
        let empty = empty.as_object().unwrap();
        assert!(
            optional_string_enum("a2a.deliver", empty, "r", allowed).is_ok(),
            "absent field must be Ok; the let-else short-circuits before the type and enum checks so unscoped a2a grants do not fail at the duplicate_risk gate",
        );

        for value in ["idempotent", "operator-accepted", "operator_accepted"] {
            let json = serde_json::json!({ "r": value });
            let obj = json.as_object().unwrap();
            assert!(
                optional_string_enum("a2a.deliver", obj, "r", allowed).is_ok(),
                "{{\"r\": {value:?}}} must be Ok; otherwise the duplicate_risk gate silently rejects an explicitly documented accepted value, breaking a2a deliveries that bind it",
            );
        }

        let unsupported = serde_json::json!({ "r": "unknown" });
        let unsupported = unsupported.as_object().unwrap();
        let err = optional_string_enum("a2a.deliver", unsupported, "r", allowed).unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("unsupported value")),
            "{{\"r\": \"unknown\"}} must produce the 'unsupported value' error; got {err:?}. A regression that broadened the allowed list to include arbitrary strings would silently authorize unsupported duplicate-risk classes, weakening operator-accepted-only delivery paths into accept-anything paths.",
        );

        let non_string = serde_json::json!({ "r": 42 });
        let non_string = non_string.as_object().unwrap();
        let err = optional_string_enum("a2a.deliver", non_string, "r", allowed).unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("must be a string")),
            "{{\"r\": 42}} must produce the 'must be a string' error via as_str() returning None; got {err:?}. A regression that fell through to the contains check on a non-string would either panic or silently bypass the enum gate for partially-typed scope objects.",
        );

        let null_value = serde_json::json!({ "r": null });
        let null_value = null_value.as_object().unwrap();
        let err = optional_string_enum("a2a.deliver", null_value, "r", allowed).unwrap_err();
        assert!(
            matches!(&err, PermissionError::InvalidScope(msg) if msg.contains("must be a string")),
            "{{\"r\": null}} must produce the 'must be a string' error via as_str() returning None on a JSON null; got {err:?}. This is the strict-deny path that distinguishes optional_string_enum from optional_string_enum_or_null — a2a.duplicate_risk has no documented unbounded marker, and a regression that accepted null would silently merge the two helpers' contracts and start treating null as 'unset' across the duplicate-risk gate.",
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

    #[test]
    fn friendly_action_title_pins_twenty_two_entry_catalog_and_none_on_unknown() {
        // covenant_permissions::friendly_action_title (lib.rs line
        // 1408-1432) maps every signed capability action emitted by
        // the daemon to a plain-English lowercase verb phrase. The
        // catalog is the operator-facing CLI rendering used by the
        // bootstrap and capability-list outputs (covenant/src/main.rs)
        // and is documented as cross-bound to
        // covenant-web/lib/labels.ts (different casing — TS uses
        // title case for card titles, Rust uses lowercase verb
        // phrases for inline CLI substitution; the action keys must
        // match across both surfaces). The function has no test
        // today, so a refactor that renamed any title would silently
        // shift every CLI rendering and drift away from the
        // covenant-web cross-binding.
        //
        // A refactor that changed the return type from
        // Option<&'static str> to &'static str (returning a default
        // for missing entries under a 'callers always get a
        // renderable string' rationale) would silently hide
        // unknown-action regressions; the None contract is the
        // documented signal that a caller should fall back to the
        // raw action.
        let catalog: &[(&str, &str)] = &[
            ("intent.subscribe", "receive your tasks"),
            ("intent.publish", "send tasks to other agents"),
            ("memory.read", "read memory"),
            ("memory.write", "save to memory"),
            ("memory.purge", "delete memories"),
            ("memory.search", "search memory"),
            ("identity.read", "see identity info"),
            ("identity.rotate", "rotate identity keys"),
            ("tool.web_search", "search the web"),
            ("tool.summarize", "summarize text"),
            ("tool.terminal", "run terminal commands"),
            ("tool.file_read", "read files"),
            ("tool.file_write", "write files"),
            ("tool.gpu_inference", "use GPU inference"),
            ("agent.spawn", "start other agents"),
            ("agent.suspend", "pause other agents"),
            ("chain.receipts", "read settlement receipts"),
            ("chain.flush", "flush receipts on-chain"),
            ("audit.purge", "purge audit log entries"),
            ("capabilities.purge", "purge revoked permissions"),
            ("peers.purge", "purge revoked peers"),
            ("a2a.compact", "compact the agent-to-agent log"),
        ];

        assert_eq!(
            catalog.len(),
            22,
            "friendly_action_title catalog has 22 documented entries — \
             a refactor that removed one (e.g., dropping 'a2a.compact' \
             under an 'a2a actions are admin-only and do not need \
             friendly titles' rationale) would silently make that \
             action fall back to the raw string in operator CLI \
             rendering; a refactor that added a 23rd entry must update \
             this count in lockstep so the catalog change is \
             intentional",
        );

        for (action, expected) in catalog {
            assert_eq!(
                friendly_action_title(action),
                Some(*expected),
                "friendly_action_title({action:?}) must return \
                 Some({expected:?}) — the catalog is the operator-\
                 facing CLI rendering used by bootstrap and \
                 capability-list outputs and is cross-bound to \
                 covenant-web/lib/labels.ts via the doc-comment. A \
                 refactor that renamed any title (e.g., 'save to \
                 memory' to 'store memory' under a 'more descriptive \
                 verb' rationale) would silently shift the CLI \
                 rendering without bumping the catalog version; the \
                 covenant-web side carries title-case variants that \
                 must stay in lockstep with the action keys here",
            );
        }

        assert_eq!(
            friendly_action_title("unknown.action"),
            None,
            "unknown action strings must return None — the documented \
             contract is that callers fall back to the raw action when \
             no friendly title is registered. A refactor that changed \
             the return type to &'static str (returning the input \
             verbatim or a default like 'unknown action') would \
             silently hide every future action that lands without a \
             catalog entry; the None contract is the load-bearing \
             signal that lets CLI callers branch on absence",
        );
        assert_eq!(
            friendly_action_title(""),
            None,
            "empty action string must return None — pins the fallthrough \
             arm of the match expression. A refactor that special-cased \
             the empty string to return a default friendly title would \
             silently mask malformed grant flows that produce empty \
             action strings; today they surface as 'no friendly title' \
             at the CLI rendering site",
        );
    }

    #[test]
    fn permission_error_display_messages_pin_four_string_variant_format_strings() {
        let expired = format!("{}", PermissionError::Expired(1700000000));
        assert_eq!(
            expired, "capability expired at 1700000000",
            "PermissionError::Expired Display drifted (typo or dropped 'capability' prefix regression class)"
        );

        let bad_sig = format!("{}", PermissionError::BadSignature);
        assert_eq!(
            bad_sig, "signature does not verify against granted_by pubkey",
            "PermissionError::BadSignature Display drifted (typo or dropped 'granted_by pubkey' qualifier regression class)"
        );

        let untrusted = format!("{}", PermissionError::UntrustedGrantor);
        assert_eq!(
            untrusted, "granted_by pubkey does not match the daemon trust root",
            "PermissionError::UntrustedGrantor Display drifted (typo or dropped 'daemon trust root' qualifier regression class)"
        );
        assert_ne!(
            untrusted, bad_sig,
            "PermissionError::UntrustedGrantor must not converge with PermissionError::BadSignature \
             (prefix-convergence regression class would merge cryptographic-rejection with trust-policy-rejection)"
        );

        let invalid_scope = format!(
            "{}",
            PermissionError::InvalidScope("memory.read: scope must include namespace".into())
        );
        assert_eq!(
            invalid_scope, "invalid capability scope: memory.read: scope must include namespace",
            "PermissionError::InvalidScope Display drifted (typo or dropped 'capability scope' prefix regression class)"
        );
    }

    #[test]
    fn permission_error_io_and_serde_and_ed25519_display_messages_pin_prefix_and_external_source_display_delegation(
    ) {
        let io_err = PermissionError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "caps.jsonl missing",
        ));
        let io_message = format!("{io_err}");
        assert!(
            io_message.starts_with("io: "),
            "PermissionError::Io must surface the literal 'io: ' bootstrap-stage prefix so audit-log filters can distinguish capability-file disk faults from JSON-parse faults, crypto faults, and the four security-relevant string surfaces (dropped-prefix regression class): {io_message}"
        );
        assert!(
            io_message.contains("caps.jsonl missing"),
            "PermissionError::Io must surface the inner std::io::Error Display rendering after the colon ({{0}}, not {{0:?}}) (Debug-vs-Display formatting regression class on the {{0}} interpolation): {io_message}"
        );
        assert!(
            !io_message.contains("Custom {") && !io_message.contains("Os {"),
            "PermissionError::Io must NOT surface the std::io::Error Debug rendering (Debug-vs-Display formatting regression class on the {{0}} interpolation): {io_message}"
        );

        let serde_source =
            serde_json::from_str::<serde_json::Value>("not json").expect_err("parse must fail");
        let serde_err = PermissionError::Serde(serde_source);
        let serde_message = format!("{serde_err}");
        assert!(
            serde_message.starts_with("serde: "),
            "PermissionError::Serde must surface the literal 'serde: ' bootstrap-stage prefix so audit-log filters can distinguish capability JSON-parse faults from disk faults and crypto faults (dropped-prefix regression class): {serde_message}"
        );
        assert!(
            serde_message.contains("expected"),
            "PermissionError::Serde must surface the inner serde_json::Error Display rendering after the colon (Debug-vs-Display formatting regression class on the {{0}} interpolation): {serde_message}"
        );
        assert!(
            !serde_message.contains("Error("),
            "PermissionError::Serde must NOT surface the serde_json::Error Debug rendering (Debug-vs-Display formatting regression class on the {{0}} interpolation): {serde_message}"
        );

        // Produce a SignatureError via a verify_strict failure: sign one
        // message, then verify against a different message body. This is
        // the simplest reliable path — VerifyingKey::from_bytes accepts
        // most malformed-looking byte patterns when they happen to decode
        // to a valid Edwards point.
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let signature = ed25519_dalek::Signer::sign(&signing_key, b"original message");
        let verifying_key = signing_key.verifying_key();
        let crypto_source = verifying_key
            .verify_strict(b"different message", &signature)
            .expect_err("verify_strict must fail on mismatched message");
        let crypto_err = PermissionError::Crypto(crypto_source);
        let crypto_message = format!("{crypto_err}");
        assert!(
            crypto_message.starts_with("ed25519: "),
            "PermissionError::Crypto must surface the literal 'ed25519: ' bootstrap-stage prefix so audit-log filters can distinguish capability crypto faults (malformed pubkey/signature bytes, signature verification failure) from disk faults, JSON-parse faults, and the four security-relevant string surfaces (dropped-prefix regression class): {crypto_message}"
        );
        assert!(
            crypto_message.len() > "ed25519: ".len(),
            "PermissionError::Crypto must surface the inner ed25519_dalek::SignatureError Display rendering after the colon ({{0}}, not {{0:?}}); a Debug refactor would render the SignatureError struct fields and a 'less verbose' refactor that dropped {{0}} entirely would leave only the prefix (dropped-source-rendering regression class on the {{0}} interpolation): {crypto_message}"
        );

        assert_ne!(
            io_message, serde_message,
            "PermissionError::Io and PermissionError::Serde Display must not converge (prefix-convergence regression class): io={io_message} serde={serde_message}"
        );
        assert_ne!(
            io_message, crypto_message,
            "PermissionError::Io and PermissionError::Crypto Display must not converge (prefix-convergence regression class): io={io_message} crypto={crypto_message}"
        );
        assert_ne!(
            serde_message, crypto_message,
            "PermissionError::Serde and PermissionError::Crypto Display must not converge (prefix-convergence regression class): serde={serde_message} crypto={crypto_message}"
        );
        assert!(
            !io_message.starts_with("serde:") && !io_message.starts_with("ed25519:"),
            "PermissionError::Io must not start with 'serde:' or 'ed25519:' (sibling-prefix-swap regression class): {io_message}"
        );
        assert!(
            !serde_message.starts_with("io:") && !serde_message.starts_with("ed25519:"),
            "PermissionError::Serde must not start with 'io:' or 'ed25519:' (sibling-prefix-swap regression class): {serde_message}"
        );
        assert!(
            !crypto_message.starts_with("io:") && !crypto_message.starts_with("serde:"),
            "PermissionError::Crypto must not start with 'io:' or 'serde:' (sibling-prefix-swap regression class): {crypto_message}"
        );

        let security_surfaces = [
            "capability expired at",
            "signature does not verify against granted_by pubkey",
            "granted_by pubkey does not match the daemon trust root",
            "invalid capability scope:",
        ];
        for surface in security_surfaces {
            assert!(
                !io_message.starts_with(surface),
                "PermissionError::Io must not converge with the security-relevant string surface '{surface}' (security-surface-convergence regression class): {io_message}"
            );
            assert!(
                !serde_message.starts_with(surface),
                "PermissionError::Serde must not converge with the security-relevant string surface '{surface}' (security-surface-convergence regression class): {serde_message}"
            );
            assert!(
                !crypto_message.starts_with(surface),
                "PermissionError::Crypto must not converge with the security-relevant string surface '{surface}' (security-surface-convergence regression class): {crypto_message}"
            );
        }
    }

    #[test]
    fn permission_error_io_source_delegation_pin_returns_inner_std_io_error_via_std_error_source() {
        use std::error::Error;

        let inner = std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "capabilities.jsonl: read denied",
        );
        let expected_display = format!("{inner}");
        let err = PermissionError::Io(inner);
        let source = err.source().expect(
            "covenant_permissions::PermissionError::Io must surface the inner std::io::Error via std::error::Error::source so daemon-side capability-load retry-policy classifiers can walk the error chain and downcast source() to std::io::Error to extract io::ErrorKind for distinct retry decisions on capability-store IO (NotFound stops capability dispatch with operator-attention, PermissionDenied escalates as a security-sensitive incident on the granted-capabilities file, Interrupted retries immediately); a refactor that converted the variant from #[from] to a hand-written Error impl returning None (under a 'simpler error wrapping' rationale) would silently change source() to return None while leaving Display intact (dropped-source-attribute regression class)",
        );
        assert_eq!(
            format!("{source}"),
            expected_display,
            "covenant_permissions::PermissionError::Io source() Display must match a direct format!() of the same std::io::Error verbatim; a refactor that swapped the inner field type to Box<dyn Error + Send + Sync> or any other wrapper would silently break daemon-side downcasts even though the wrapper's Display would continue to flow through {{0}} (concrete-source-type regression class)"
        );
        let kind = source.downcast_ref::<std::io::Error>().map(|e| e.kind());
        assert_eq!(
            kind,
            Some(std::io::ErrorKind::PermissionDenied),
            "covenant_permissions::PermissionError::Io source() must downcast_ref to std::io::Error so daemon-side capability-load retry-policy classifiers can extract io::ErrorKind for retry decisions on capability-store IO; a refactor that wrapped the inner in a project-local newtype (e.g., PermissionIoError(std::io::Error) under a 'tag capability-store IO failures distinctly from sibling Io variants in other crates' rationale) would silently break downcast_ref::<std::io::Error>() at every downstream callsite that classifies capability-store IO faults — particularly relevant on the security-sensitive granted-capabilities path (concrete-source-type downcast regression class)"
        );
    }

    #[test]
    fn permission_error_crypto_source_delegation_pin_returns_inner_ed25519_signature_error_via_std_error_source(
    ) {
        use std::error::Error;

        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let signature = ed25519_dalek::Signer::sign(&signing_key, b"original message");
        let verifying_key = signing_key.verifying_key();
        let inner = verifying_key
            .verify_strict(b"different message", &signature)
            .expect_err("verify_strict must fail on mismatched message");
        let expected_display = format!("{inner}");
        let err = PermissionError::Crypto(inner);
        let source = err.source().expect(
            "PermissionError::Crypto must surface the inner ed25519_dalek::SignatureError via std::error::Error::source so daemon-side capability-verification audit emitters can walk the error chain and downcast source() to ed25519_dalek::SignatureError for distinct triage of 'malformed pubkey bytes' (key-decode failure path) vs 'signature verification failed' (cryptographic mismatch path); a refactor that converted the variant from #[from] to a hand-written Error impl returning None (under a 'simpler error wrapping' rationale) would silently change source() to return None while leaving Display intact (dropped-source-attribute regression class)",
        );
        assert_eq!(
            format!("{source}"),
            expected_display,
            "PermissionError::Crypto source() Display must match a direct format!() of the same ed25519_dalek::SignatureError verbatim; a refactor that swapped the inner field type to Box<dyn Error + Send + Sync> or any other wrapper would silently break daemon-side downcasts even though the wrapper's Display would continue to flow through {{0}} (concrete-source-type regression class)"
        );
        assert!(
            source.downcast_ref::<ed25519_dalek::SignatureError>().is_some(),
            "PermissionError::Crypto source() must downcast_ref to ed25519_dalek::SignatureError so daemon-side capability-verification audit emitters can extract the concrete crypto-failure type for triage; a refactor that wrapped the inner in a project-local newtype (e.g., PermissionCryptoError(ed25519_dalek::SignatureError) under a 'distinguish capability-verification crypto failures from sibling crypto-error sites' rationale) would silently break downcast_ref::<ed25519_dalek::SignatureError>() at every downstream callsite (concrete-source-type downcast regression class)"
        );
    }

    #[test]
    fn permission_error_serde_source_delegation_pin_returns_inner_serde_json_error_via_std_error_source(
    ) {
        use std::error::Error;

        let inner = serde_json::from_str::<serde_json::Value>("not json")
            .expect_err("parse must fail");
        let expected_display = format!("{inner}");
        let err = PermissionError::Serde(inner);
        let source = err.source().expect(
            "PermissionError::Serde must surface the inner serde_json::Error via std::error::Error::source so daemon-side capability-document diagnostics can walk the error chain and downcast source() to serde_json::Error to inspect line/column or classify() for malformed-capability-document identification (line/column points the operator at the offending capability JSON, classify() distinguishes Syntax-vs-Data-vs-EOF for incident triage on a malformed grant); a refactor that converted the variant from #[from] to a hand-written Error impl returning None (under a 'simpler error wrapping' rationale) would silently change source() to return None while leaving Display intact (dropped-source-attribute regression class)",
        );
        assert_eq!(
            format!("{source}"),
            expected_display,
            "PermissionError::Serde source() Display must match a direct format!() of the same serde_json::Error verbatim; a refactor that swapped the inner field type to Box<dyn Error + Send + Sync> or any other wrapper would silently break daemon-side downcasts even though the wrapper's Display would continue to flow through {{0}} (concrete-source-type regression class)"
        );
        assert!(
            source.downcast_ref::<serde_json::Error>().is_some(),
            "PermissionError::Serde source() must downcast_ref to serde_json::Error so daemon-side capability-document diagnostics can call serde_json::Error::line/column/classify for malformed-capability identification; a refactor that wrapped the inner in a project-local newtype (e.g., PermissionSerdeError(serde_json::Error) under a 'consolidate parse errors into one Wire variant' rationale) would silently break downcast_ref::<serde_json::Error>() at every downstream callsite that classifies capability-document parse faults (concrete-source-type downcast regression class)"
        );
    }
}
