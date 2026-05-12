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
        assert!(
            !a2a_scope_allows("a2a.send.peer", &scope, "a2a.send.peer", actual_none).unwrap()
        );
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
        assert!(a2a_scope_allows(
            "a2a.requeue",
            &underscore_scope,
            "a2a.requeue",
            dash_actual
        )
        .unwrap());

        let dash_scope =
            serde_json::json!({ "version": 1, "duplicate_risk": "operator-accepted" });
        let underscore_actual = A2aScopeRequest {
            duplicate_risk: Some("operator_accepted"),
            ..A2aScopeRequest::default()
        };
        assert!(a2a_scope_allows(
            "a2a.requeue",
            &dash_scope,
            "a2a.requeue",
            underscore_actual
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
        assert!(
            !peer_scope_allows("peers.purge", &scope, "peers.purge", actual_none).unwrap()
        );
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
        assert!(
            memory_purge_scope_allows("memory.purge", &scope, Some("working"), 1_000).unwrap()
        );
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
        assert!(
            !memory_repair_scope_allows(
                "memory",
                &serde_json::json!([]),
                "record-1",
                "working",
                0,
                true
            )
            .unwrap()
        );

        let bound_scope = serde_json::json!({
            "version": 1,
            "record_id": "record-1",
            "tiers": ["working"],
            "before_ms": 1_000,
            "apply": true
        });
        assert!(
            !memory_repair_scope_allows(
                "memory.repair.dry_run",
                &bound_scope,
                "record-1",
                "working",
                999,
                true
            )
            .unwrap()
        );

        let empty = serde_json::json!({});
        assert!(
            memory_repair_scope_allows(
                "memory.repair.apply",
                &empty,
                "any-record",
                "working",
                0,
                true
            )
            .unwrap()
        );
        assert!(
            memory_repair_scope_allows(
                "memory.repair.apply",
                &empty,
                "any-record",
                "longterm",
                u64::MAX,
                true
            )
            .unwrap()
        );
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
        assert!(
            !memory_compaction_scope_allows(
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
            .unwrap()
        );

        let bound_scope = serde_json::json!({
            "version": 1,
            "tiers": ["working", "episodic"],
            "before_ms": 1_000,
            "apply": true
        });
        assert!(
            !memory_compaction_scope_allows(
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
            .unwrap()
        );

        let empty = serde_json::json!({});
        assert!(
            memory_compaction_scope_allows(
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
            .unwrap()
        );
        assert!(
            memory_compaction_scope_allows(
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
            .unwrap()
        );
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
        assert!(
            !memory_compaction_scope_allows(
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
            .unwrap()
        );
        assert!(
            !memory_compaction_scope_allows(
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
            .unwrap()
        );
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
        let err =
            optional_non_negative_integer_or_null("audit.verify", negative, "x").unwrap_err();
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
        let err =
            optional_string_array("memory.read", non_string_entry, "x", allowed).unwrap_err();
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
}
