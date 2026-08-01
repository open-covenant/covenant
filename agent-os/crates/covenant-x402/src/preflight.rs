//! Deterministic, advisory preflight for one narrow x402 payment profile.
//!
//! This module evaluates a proposed Solana USDC payment against a trusted
//! local policy. It does not build or inspect a transaction, hold a key,
//! authorize a signer, reserve a one-use grant, submit a transaction, or
//! verify settlement. [`PreflightReceiptV1`] encodes that boundary with
//! false-only flags so a v1 receipt cannot represent signer enforcement.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use url::Url;

pub const SOLANA_MAINNET: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";
pub const SOLANA_DEVNET: &str = "solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1";
pub const USDC_MAINNET_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
pub const USDC_DEVNET_MINT: &str = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU";
pub const SPL_TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

const INTENT_HASH_DOMAIN: &[u8] = b"covenant.payment-intent.v1";
const POLICY_HASH_DOMAIN: &[u8] = b"covenant.payment-policy.v1";
const JSON_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug, thiserror::Error)]
pub enum PreflightError {
    #[error("canonical JSON: {0}")]
    CanonicalJson(#[from] serde_json::Error),
    #[error("{field} exceeds the JSON safe-integer maximum: {value}")]
    JsonSafeInteger { field: &'static str, value: u64 },
}

mod json_safe_u64 {
    use super::JSON_SAFE_INTEGER;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        if *value > JSON_SAFE_INTEGER {
            return Err(serde::ser::Error::custom(format!(
                "integer exceeds JSON safe-integer maximum {JSON_SAFE_INTEGER}"
            )));
        }
        serializer.serialize_u64(*value)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        let value = u64::deserialize(deserializer)?;
        if value > JSON_SAFE_INTEGER {
            return Err(serde::de::Error::custom(format!(
                "integer exceeds JSON safe-integer maximum {JSON_SAFE_INTEGER}"
            )));
        }
        Ok(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaymentIntentSchema {
    #[serde(rename = "covenant.payment-intent.v1")]
    V1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaymentPolicySchema {
    #[serde(rename = "covenant.payment-policy.v1")]
    V1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PreflightReceiptSchema {
    #[serde(rename = "covenant.preflight-receipt.v1")]
    V1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PaymentProtocolName {
    X402,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PaymentScheme {
    Exact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionProfile {
    SponsoredV0StaticV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolBindingV1 {
    pub name: PaymentProtocolName,
    pub version: u16,
    pub scheme: PaymentScheme,
    pub accepted_sha256: String,
    pub payment_identifier: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestBindingV1 {
    pub url: String,
    pub method: String,
    pub body_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolanaPaymentV1 {
    pub network: String,
    pub funder: String,
    pub fee_payer: String,
    pub mint: String,
    pub token_program: String,
    pub required_amount: String,
    pub transfer_amount: String,
    pub decimals: u8,
    pub pay_to: String,
    pub source_ata: String,
    pub destination_ata: String,
    pub memo: String,
    pub compute_unit_limit: u32,
    #[serde(with = "json_safe_u64")]
    pub compute_unit_price_micro_lamports: u64,
    pub transaction_profile: TransactionProfile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaymentIntentV1 {
    pub schema: PaymentIntentSchema,
    pub protocol: ProtocolBindingV1,
    pub request: RequestBindingV1,
    pub payment: SolanaPaymentV1,
    #[serde(with = "json_safe_u64")]
    pub observed_at_ms: u64,
    #[serde(with = "json_safe_u64")]
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaymentRouteV1 {
    pub origin: String,
    pub method: String,
    pub path_and_query: String,
    pub pay_to: String,
    pub destination_ata: String,
    pub max_amount: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrueFlag;

impl Serialize for TrueFlag {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bool(true)
    }
}

impl<'de> Deserialize<'de> for TrueFlag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if bool::deserialize(deserializer)? {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom("expected true"))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaymentPolicyV1 {
    pub schema: PaymentPolicySchema,
    pub network: String,
    pub mint: String,
    pub token_program: String,
    pub decimals: u8,
    pub funder: String,
    pub source_ata: String,
    pub allowed_fee_payers: Vec<String>,
    pub routes: Vec<PaymentRouteV1>,
    pub max_compute_unit_limit: u32,
    #[serde(with = "json_safe_u64")]
    pub max_compute_unit_price_micro_lamports: u64,
    #[serde(with = "json_safe_u64")]
    pub max_intent_lifetime_ms: u64,
    pub transaction_profile: TransactionProfile,
    pub require_exact_amount: TrueFlag,
    pub require_memo_equals_payment_identifier: TrueFlag,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightOutcome {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightReasonCode {
    PolicyInvalid,
    UnsupportedX402Version,
    DigestInvalid,
    PaymentIdentifierInvalid,
    ResourceNotAllowed,
    NetworkNotAllowed,
    MintNotAllowed,
    TokenProgramNotAllowed,
    DecimalsMismatch,
    FunderMismatch,
    FeePayerNotAllowed,
    PayToMismatch,
    SourceAtaMismatch,
    DestinationAtaMismatch,
    SolanaAddressInvalid,
    AmountInvalid,
    AmountMismatch,
    AmountExceedsLimit,
    MemoInvalid,
    ComputeBudgetExceeded,
    TimeWindowInvalid,
    NotYetValid,
    Expired,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FalseFlag;

impl Serialize for FalseFlag {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bool(false)
    }
}

impl<'de> Deserialize<'de> for FalseFlag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if bool::deserialize(deserializer)? {
            Err(serde::de::Error::custom("expected false"))
        } else {
            Ok(Self)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnforcementModeV1 {
    Advisory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdvisoryEnforcementV1 {
    pub mode: EnforcementModeV1,
    pub decision_consumed_by_signer: FalseFlag,
    pub signing_key_isolated: FalseFlag,
    pub durable_single_use: FalseFlag,
}

impl Default for AdvisoryEnforcementV1 {
    fn default() -> Self {
        Self {
            mode: EnforcementModeV1::Advisory,
            decision_consumed_by_signer: FalseFlag,
            signing_key_isolated: FalseFlag,
            durable_single_use: FalseFlag,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreflightReceiptV1 {
    pub schema: PreflightReceiptSchema,
    pub intent: PaymentIntentV1,
    pub intent_sha256: String,
    pub policy_sha256: String,
    pub outcome: PreflightOutcome,
    pub reason_codes: Vec<PreflightReasonCode>,
    #[serde(with = "json_safe_u64")]
    pub evaluated_at_ms: u64,
    pub enforcement: AdvisoryEnforcementV1,
}

pub fn payment_intent_hash(intent: &PaymentIntentV1) -> Result<String, PreflightError> {
    ensure_intent_safe_integers(intent)?;
    canonical_hash(INTENT_HASH_DOMAIN, intent)
}

pub fn payment_policy_hash(policy: &PaymentPolicyV1) -> Result<String, PreflightError> {
    ensure_policy_safe_integers(policy)?;
    canonical_hash(POLICY_HASH_DOMAIN, policy)
}

pub fn evaluate_preflight(
    intent: &PaymentIntentV1,
    policy: &PaymentPolicyV1,
    evaluated_at_ms: u64,
) -> Result<PreflightReceiptV1, PreflightError> {
    ensure_intent_safe_integers(intent)?;
    ensure_policy_safe_integers(policy)?;
    ensure_safe_integer("evaluated_at_ms", evaluated_at_ms)?;

    let mut reasons = Vec::new();
    let policy_valid = validate_policy(policy);
    if !policy_valid {
        push_reason(&mut reasons, PreflightReasonCode::PolicyInvalid);
    }

    if intent.protocol.version != 2 {
        push_reason(&mut reasons, PreflightReasonCode::UnsupportedX402Version);
    }
    if !valid_sha256(&intent.protocol.accepted_sha256) || !valid_sha256(&intent.request.body_sha256)
    {
        push_reason(&mut reasons, PreflightReasonCode::DigestInvalid);
    }
    if !valid_payment_identifier(&intent.protocol.payment_identifier) {
        push_reason(&mut reasons, PreflightReasonCode::PaymentIdentifierInvalid);
    }

    let route = canonical_request(&intent.request).and_then(|(origin, method, target)| {
        policy.routes.iter().find(|route| {
            route.origin == origin && route.method == method && route.path_and_query == target
        })
    });
    if route.is_none() {
        push_reason(&mut reasons, PreflightReasonCode::ResourceNotAllowed);
    }

    if intent.payment.network != policy.network {
        push_reason(&mut reasons, PreflightReasonCode::NetworkNotAllowed);
    }
    if intent.payment.mint != policy.mint {
        push_reason(&mut reasons, PreflightReasonCode::MintNotAllowed);
    }
    if intent.payment.token_program != policy.token_program {
        push_reason(&mut reasons, PreflightReasonCode::TokenProgramNotAllowed);
    }
    if intent.payment.decimals != policy.decimals {
        push_reason(&mut reasons, PreflightReasonCode::DecimalsMismatch);
    }
    if intent.payment.funder != policy.funder {
        push_reason(&mut reasons, PreflightReasonCode::FunderMismatch);
    }
    if !policy
        .allowed_fee_payers
        .contains(&intent.payment.fee_payer)
    {
        push_reason(&mut reasons, PreflightReasonCode::FeePayerNotAllowed);
    }
    if intent.payment.source_ata != policy.source_ata {
        push_reason(&mut reasons, PreflightReasonCode::SourceAtaMismatch);
    }
    if intent.payment.transaction_profile != policy.transaction_profile {
        push_reason(&mut reasons, PreflightReasonCode::PolicyInvalid);
    }

    if [
        intent.payment.funder.as_str(),
        intent.payment.fee_payer.as_str(),
        intent.payment.mint.as_str(),
        intent.payment.token_program.as_str(),
        intent.payment.pay_to.as_str(),
        intent.payment.source_ata.as_str(),
        intent.payment.destination_ata.as_str(),
    ]
    .iter()
    .any(|address| !valid_solana_address(address))
    {
        push_reason(&mut reasons, PreflightReasonCode::SolanaAddressInvalid);
    }

    let required = parse_atomic_amount(&intent.payment.required_amount);
    let transfer = parse_atomic_amount(&intent.payment.transfer_amount);
    if required.is_none() || transfer.is_none() {
        push_reason(&mut reasons, PreflightReasonCode::AmountInvalid);
    } else if required != transfer {
        push_reason(&mut reasons, PreflightReasonCode::AmountMismatch);
    }

    if let Some(route) = route {
        if intent.payment.pay_to != route.pay_to {
            push_reason(&mut reasons, PreflightReasonCode::PayToMismatch);
        }
        if intent.payment.destination_ata != route.destination_ata {
            push_reason(&mut reasons, PreflightReasonCode::DestinationAtaMismatch);
        }
        match (transfer, parse_atomic_amount(&route.max_amount)) {
            (Some(amount), Some(max)) if amount > max => {
                push_reason(&mut reasons, PreflightReasonCode::AmountExceedsLimit)
            }
            (_, None) => push_reason(&mut reasons, PreflightReasonCode::PolicyInvalid),
            _ => {}
        }
    }

    if intent.payment.memo != intent.protocol.payment_identifier
        || !(16..=256).contains(&intent.payment.memo.len())
    {
        push_reason(&mut reasons, PreflightReasonCode::MemoInvalid);
    }
    if intent.payment.compute_unit_limit == 0
        || intent.payment.compute_unit_limit > policy.max_compute_unit_limit
        || intent.payment.compute_unit_price_micro_lamports == 0
        || intent.payment.compute_unit_price_micro_lamports > JSON_SAFE_INTEGER
        || intent.payment.compute_unit_price_micro_lamports
            > policy.max_compute_unit_price_micro_lamports
    {
        push_reason(&mut reasons, PreflightReasonCode::ComputeBudgetExceeded);
    }

    if intent.observed_at_ms > JSON_SAFE_INTEGER
        || intent.expires_at_ms > JSON_SAFE_INTEGER
        || evaluated_at_ms > JSON_SAFE_INTEGER
        || intent.observed_at_ms >= intent.expires_at_ms
        || intent.expires_at_ms.saturating_sub(intent.observed_at_ms)
            > policy.max_intent_lifetime_ms
    {
        push_reason(&mut reasons, PreflightReasonCode::TimeWindowInvalid);
    }
    if evaluated_at_ms < intent.observed_at_ms {
        push_reason(&mut reasons, PreflightReasonCode::NotYetValid);
    }
    if evaluated_at_ms >= intent.expires_at_ms {
        push_reason(&mut reasons, PreflightReasonCode::Expired);
    }

    Ok(PreflightReceiptV1 {
        schema: PreflightReceiptSchema::V1,
        intent: intent.clone(),
        intent_sha256: payment_intent_hash(intent)?,
        policy_sha256: payment_policy_hash(policy)?,
        outcome: if reasons.is_empty() {
            PreflightOutcome::Allow
        } else {
            PreflightOutcome::Deny
        },
        reason_codes: reasons,
        evaluated_at_ms,
        enforcement: AdvisoryEnforcementV1::default(),
    })
}

fn ensure_safe_integer(field: &'static str, value: u64) -> Result<(), PreflightError> {
    if value > JSON_SAFE_INTEGER {
        return Err(PreflightError::JsonSafeInteger { field, value });
    }
    Ok(())
}

fn ensure_intent_safe_integers(intent: &PaymentIntentV1) -> Result<(), PreflightError> {
    ensure_safe_integer(
        "payment.compute_unit_price_micro_lamports",
        intent.payment.compute_unit_price_micro_lamports,
    )?;
    ensure_safe_integer("observed_at_ms", intent.observed_at_ms)?;
    ensure_safe_integer("expires_at_ms", intent.expires_at_ms)
}

fn ensure_policy_safe_integers(policy: &PaymentPolicyV1) -> Result<(), PreflightError> {
    ensure_safe_integer(
        "max_compute_unit_price_micro_lamports",
        policy.max_compute_unit_price_micro_lamports,
    )?;
    ensure_safe_integer("max_intent_lifetime_ms", policy.max_intent_lifetime_ms)
}

/// Replays a receipt's evaluation against the policy it names by hash.
///
/// A verifier must possess the exact policy document; the receipt deliberately
/// carries only its hash so local policy contents are not disclosed by default.
pub fn verify_preflight_receipt(
    receipt: &PreflightReceiptV1,
    policy: &PaymentPolicyV1,
) -> Result<bool, PreflightError> {
    Ok(evaluate_preflight(&receipt.intent, policy, receipt.evaluated_at_ms)? == *receipt)
}

fn validate_policy(policy: &PaymentPolicyV1) -> bool {
    let supported_pair = matches!(
        (policy.network.as_str(), policy.mint.as_str()),
        (SOLANA_MAINNET, USDC_MAINNET_MINT) | (SOLANA_DEVNET, USDC_DEVNET_MINT)
    );
    if !supported_pair
        || policy.token_program != SPL_TOKEN_PROGRAM
        || policy.decimals != 6
        || !valid_solana_address(&policy.funder)
        || !valid_solana_address(&policy.source_ata)
        || policy.allowed_fee_payers.is_empty()
        || policy.routes.is_empty()
        || policy.max_compute_unit_limit == 0
        || policy.max_compute_unit_price_micro_lamports == 0
        || policy.max_compute_unit_price_micro_lamports > JSON_SAFE_INTEGER
        || policy.max_intent_lifetime_ms == 0
        || policy.max_intent_lifetime_ms > JSON_SAFE_INTEGER
    {
        return false;
    }

    if policy
        .allowed_fee_payers
        .iter()
        .any(|address| !valid_solana_address(address))
    {
        return false;
    }
    let unique_fee_payers = policy.allowed_fee_payers.iter().collect::<HashSet<_>>();
    if unique_fee_payers.len() != policy.allowed_fee_payers.len() {
        return false;
    }

    let unique_routes = policy
        .routes
        .iter()
        .map(|route| (&route.origin, &route.method, &route.path_and_query))
        .collect::<HashSet<_>>();
    if unique_routes.len() != policy.routes.len() {
        return false;
    }

    policy.routes.iter().all(|route| {
        valid_policy_route(route)
            && valid_solana_address(&route.pay_to)
            && valid_solana_address(&route.destination_ata)
            && parse_atomic_amount(&route.max_amount).is_some()
    })
}

fn valid_policy_route(route: &PaymentRouteV1) -> bool {
    let probe = format!("{}{}", route.origin, route.path_and_query);
    let request = RequestBindingV1 {
        url: probe,
        method: route.method.clone(),
        body_sha256: format!("sha256:{}", "0".repeat(64)),
    };
    matches!(
        canonical_request(&request),
        Some((origin, method, target))
            if origin == route.origin
                && method == route.method
                && target == route.path_and_query
    )
}

fn canonical_request(request: &RequestBindingV1) -> Option<(String, String, String)> {
    let url = Url::parse(&request.url).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    if !matches!(request.method.as_str(), "GET" | "POST") {
        return None;
    }
    let mut target = url.path().to_string();
    if let Some(query) = url.query() {
        target.push('?');
        target.push_str(query);
    }
    Some((
        url.origin().ascii_serialization(),
        request.method.clone(),
        target,
    ))
}

fn valid_sha256(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_payment_identifier(value: &str) -> bool {
    (16..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_solana_address(value: &str) -> bool {
    bs58::decode(value)
        .into_vec()
        .map(|bytes| bytes.len() == 32)
        .unwrap_or(false)
}

fn parse_atomic_amount(value: &str) -> Option<u64> {
    if value.is_empty()
        || value == "0"
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    value.parse().ok()
}

fn push_reason(reasons: &mut Vec<PreflightReasonCode>, reason: PreflightReasonCode) {
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
}

fn canonical_hash<T: Serialize>(domain: &[u8], value: &T) -> Result<String, PreflightError> {
    let canonical = serde_jcs::to_vec(value)?;
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update([0]);
    hasher.update(canonical);
    let digest = hasher.finalize();
    Ok(format!("sha256:{}", lower_hex(&digest)))
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn false_flags_reject_true() {
        let body = r#"{
            "mode":"advisory",
            "decision_consumed_by_signer":true,
            "signing_key_isolated":false,
            "durable_single_use":false
        }"#;
        let error = serde_json::from_str::<AdvisoryEnforcementV1>(body).unwrap_err();
        assert!(error.to_string().contains("expected false"));
    }

    #[test]
    fn true_flags_reject_false() {
        assert!(serde_json::from_str::<TrueFlag>("false").is_err());
    }

    #[test]
    fn atomic_amount_is_positive_canonical_u64() {
        assert_eq!(parse_atomic_amount("1"), Some(1));
        assert_eq!(parse_atomic_amount(&u64::MAX.to_string()), Some(u64::MAX));
        for invalid in ["", "0", "01", "-1", "1.0", "18446744073709551616"] {
            assert_eq!(parse_atomic_amount(invalid), None, "accepted {invalid:?}");
        }
    }
}
