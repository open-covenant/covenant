//! Structural observations over configured DAS-provider responses.
//!
//! This module compares provider-reported MPL Core AppData fields with an
//! expected authority and Covenant-specific envelope. It does not authenticate
//! a Core account, validate the committed evidence, establish log completeness,
//! identify an operator, or produce an accountability verdict. A direct account
//! decode or proof is required to remove trust in the DAS provider.
//!
//! DAS indexers re-case the on-chain camelCase keys to snake_case (Helius
//! returns `response_hash`); every read here accepts both spellings.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::COVENANT_ATTESTATION_AUTHORITY;
use crate::das::{DasClient, DasError};
use crate::request::{
    validate_attestation_field, validate_onchain_pubkey, validate_root_hash_hex,
    ATTESTATION_HASH_ALG, ATTESTATION_SCHEMA, ATTESTATION_TYPE, SUBJECT_REGISTRY,
};

const MAX_UNIX_SECONDS: u64 = 253_402_300_799;

/// Structural observation for one candidate record asset.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecordObservation {
    /// The record asset id reported by DAS, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset: Option<String>,
    /// Whether the reported envelope matches the configured field expectations.
    pub matches_expected_envelope: bool,
    /// The reported `subject.asset`, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject_asset: Option<String>,
    /// The AppData write authority reported by the configured DAS provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authority: Option<String>,
    /// The reported 32-byte commitment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recorded_at: Option<u64>,
    /// Structural mismatches, empty when the expected envelope matched.
    pub reasons: Vec<String>,
}

/// DAS-provider observation of matching records that name one agent asset.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentRecordObservation {
    pub agent: String,
    pub has_matching_record: bool,
    pub record_count: usize,
    /// The latest matching provider-reported record by its claimed timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest: Option<RecordObservation>,
}

/// The AppData external plugin on a DAS asset, if present.
fn app_data(asset: &Value) -> Option<&Value> {
    asset
        .get("external_plugins")?
        .as_array()?
        .iter()
        .find(|p| p.get("type").and_then(Value::as_str) == Some("AppData"))
}

/// Read a payload field tolerating snake_case (DAS) and camelCase (raw).
fn field<'a>(data: &'a Value, snake: &str, camel: &str) -> Option<&'a str> {
    data.get(snake)
        .or_else(|| data.get(camel))
        .and_then(Value::as_str)
}

/// Read the DAS-reported AppData `data_authority`. Helius nests it under
/// `adapter_config`; a flat fallback covers other provider shapes. The plugin's
/// top-level `authority` is the adapter configuration authority and is ignored.
fn data_authority(plugin: &Value) -> Option<&str> {
    plugin
        .get("adapter_config")
        .or_else(|| plugin.get("adapterConfig"))
        .and_then(|c| c.get("data_authority").or_else(|| c.get("dataAuthority")))
        .or_else(|| {
            plugin
                .get("data_authority")
                .or_else(|| plugin.get("dataAuthority"))
        })
        .and_then(|a| a.get("address"))
        .and_then(Value::as_str)
}

/// Inspect one DAS asset against the expected Covenant record envelope.
///
/// This pure function trusts its input as a provider report. It collects every
/// structural mismatch rather than short-circuiting.
pub fn inspect_record(asset: &Value, expected_authority: &str) -> RecordObservation {
    let mut reasons = Vec::new();
    let asset_id = asset.get("id").and_then(Value::as_str).map(str::to_string);

    let Some(plugin) = app_data(asset) else {
        return RecordObservation {
            asset: asset_id,
            matches_expected_envelope: false,
            subject_asset: None,
            authority: None,
            response_hash: None,
            recorded_at: None,
            reasons: vec!["no AppData external plugin on this asset".into()],
        };
    };
    let data = plugin.get("data").cloned().unwrap_or(Value::Null);
    let authority = data_authority(plugin).map(str::to_string);

    if field(&data, "type", "type") != Some(ATTESTATION_TYPE) {
        reasons.push(format!("type is not {ATTESTATION_TYPE}"));
    }
    if field(&data, "schema", "schema") != Some(ATTESTATION_SCHEMA) {
        reasons.push(format!("schema is not {ATTESTATION_SCHEMA}"));
    }
    if field(&data, "hash_alg", "hashAlg") != Some(ATTESTATION_HASH_ALG) {
        reasons.push(format!("hashAlg is not {ATTESTATION_HASH_ALG}"));
    }
    let response_hash = field(&data, "response_hash", "responseHash").map(str::to_string);
    match &response_hash {
        Some(h) if validate_root_hash_hex(h).is_ok() => {}
        Some(_) => reasons.push("responseHash is not 64 lowercase hex".into()),
        None => reasons.push("responseHash missing".into()),
    }

    match &authority {
        Some(a) if a == expected_authority => {}
        Some(a) => reasons.push(format!(
            "data authority {a} is not the Covenant authority {expected_authority}"
        )),
        None => reasons.push("AppData has no write authority".into()),
    }
    // The stamped validator must mirror the on-chain authority.
    if field(&data, "validator", "validator") != Some(expected_authority) {
        reasons.push("validator field does not match the expected authority".into());
    }

    let subject = data.get("subject").filter(|value| value.is_object());
    if subject.is_none() {
        reasons.push("subject object missing".into());
    }
    if subject.and_then(|value| field(value, "registry", "registry")) != Some(SUBJECT_REGISTRY) {
        reasons.push(format!("subject.registry is not {SUBJECT_REGISTRY}"));
    }
    let subject_asset = subject
        .and_then(|value| field(value, "asset", "asset"))
        .map(str::to_string);
    match &subject_asset {
        Some(asset) if validate_onchain_pubkey("subject.asset", asset).is_ok() => {}
        Some(_) => reasons.push("subject.asset is not a base58 Solana-address shape".into()),
        None => reasons.push("subject.asset missing".into()),
    }

    if let Some(registration) = subject.and_then(|value| value.get("registration")) {
        match registration.as_str() {
            Some(value) if validate_onchain_pubkey("subject.registration", value).is_ok() => {}
            _ => reasons.push("subject.registration is not a base58 Solana-address shape".into()),
        }
    }
    if let Some(agent_id) =
        subject.and_then(|value| value.get("agent_id").or_else(|| value.get("agentId")))
    {
        match agent_id.as_str() {
            Some(value)
                if !value.is_empty()
                    && validate_attestation_field("subject.agentId", value).is_ok() => {}
            _ => reasons.push("subject.agentId is not a safe non-empty string".into()),
        }
    }

    let tag = field(&data, "tag", "tag");
    match tag {
        Some(value) if !value.is_empty() && validate_attestation_field("tag", value).is_ok() => {}
        _ => reasons.push("tag missing, empty, or unsafe".into()),
    }

    let covenant = data.get("covenant").filter(|value| value.is_object());
    if covenant.is_none() {
        reasons.push("covenant object missing".into());
    }
    let release_target = covenant.and_then(|value| field(value, "release_target", "releaseTarget"));
    let release_subject =
        covenant.and_then(|value| field(value, "release_subject", "releaseSubject"));
    let release_scope = covenant.and_then(|value| field(value, "release_scope", "releaseScope"));
    for (name, value) in [
        ("covenant.releaseTarget", release_target),
        ("covenant.releaseSubject", release_subject),
        ("covenant.releaseScope", release_scope),
    ] {
        match value {
            Some(value) if !value.is_empty() && validate_attestation_field(name, value).is_ok() => {
            }
            _ => reasons.push(format!("{name} missing, empty, or unsafe")),
        }
    }
    if matches!((tag, release_scope), (Some(tag), Some(scope)) if tag != scope) {
        reasons.push("tag does not match covenant.releaseScope".into());
    }

    let recorded_value = data.get("recorded_at").or_else(|| data.get("recordedAt"));
    let recorded_at = match recorded_value.and_then(Value::as_u64) {
        Some(value) if value <= MAX_UNIX_SECONDS => Some(value),
        Some(_) => {
            reasons.push("recordedAt is outside the supported Unix-seconds range".into());
            None
        }
        None if recorded_value.is_none() => {
            reasons.push("recordedAt missing".into());
            None
        }
        None => {
            reasons.push("recordedAt is not an unsigned integer".into());
            None
        }
    };

    RecordObservation {
        asset: asset_id,
        matches_expected_envelope: reasons.is_empty(),
        subject_asset,
        authority,
        response_hash,
        recorded_at,
        reasons,
    }
}

/// Query DAS for expected-envelope records whose reported subject is
/// `agent_asset`, then return their count and latest claimed timestamp.
/// Results remain provider-backed observations, not accountability evidence.
pub async fn inspect_agent_records(
    das: &dyn DasClient,
    agent_asset: &str,
    expected_authority: &str,
) -> Result<AgentRecordObservation, DasError> {
    let mut matching: Vec<RecordObservation> = Vec::new();
    // The authority owns the attestation assets it mints; page through them.
    for page in 1..=5u32 {
        let resp = das
            .get_assets_by_owner(expected_authority, 1000, page)
            .await?;
        let items = resp
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if items.is_empty() {
            break;
        }
        for item in &items {
            let observation = inspect_record(item, expected_authority);
            if observation.matches_expected_envelope
                && observation.subject_asset.as_deref() == Some(agent_asset)
            {
                matching.push(observation);
            }
        }
        if items.len() < 1000 {
            break;
        }
    }

    let latest = matching
        .iter()
        .max_by_key(|v| v.recorded_at.unwrap_or(0))
        .cloned();
    Ok(AgentRecordObservation {
        agent: agent_asset.to_string(),
        has_matching_record: !matching.is_empty(),
        record_count: matching.len(),
        latest,
    })
}

/// The configured historical Covenant AppData authority.
pub fn default_authority() -> &'static str {
    COVENANT_ATTESTATION_AUTHORITY
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const AUTH: &str = COVENANT_ATTESTATION_AUTHORITY;
    const AGENT: &str = "4XtUrwvPWAzMGnsKenMpTMATXN3e2quJV11Jg2dab2dc";

    /// A DAS asset carrying a valid v2 attestation, snake_cased like Helius:
    /// the write authority lives in `adapter_config.data_authority`, and the
    /// adapter's own `authority` is a different (here, matching) key.
    fn das_attestation(authority: &str, validator: &str, subject: &str, schema: &str) -> Value {
        json!({
            "id": "7PEd79CG1hFUU9qeBnAKmyA77YWzckd572qsYdq3W3GH",
            "external_plugins": [{
                "type": "AppData",
                "authority": { "address": authority },
                "adapter_config": { "schema": "Json", "data_authority": { "address": authority } },
                "data": {
                    "type": ATTESTATION_TYPE,
                    "schema": schema,
                    "subject": { "registry": "mpl-agent-014", "asset": subject },
                    "validator": validator,
                    "hash_alg": "sha256-merkle",
                    "response_hash": "7c375d0e0a749966541c7543b87b76f61fd4b64d41ff12473d68f3ff45caef26",
                    "tag": "audit",
                    "covenant": {
                        "release_target": "covenant",
                        "release_subject": "witness-loop",
                        "release_scope": "audit"
                    },
                    "recorded_at": 1781738307u64
                }
            }]
        })
    }

    #[test]
    fn expected_record_envelope_matches() {
        let a = das_attestation(AUTH, AUTH, AGENT, ATTESTATION_SCHEMA);
        let v = inspect_record(&a, AUTH);
        assert!(v.matches_expected_envelope, "reasons: {:?}", v.reasons);
        assert_eq!(v.subject_asset.as_deref(), Some(AGENT));
        assert_eq!(v.authority.as_deref(), Some(AUTH));
        assert_eq!(v.response_hash.as_deref().map(|h| h.len()), Some(64));
    }

    #[test]
    fn wrong_authority_fails() {
        let other = "So11111111111111111111111111111111111111112";
        let a = das_attestation(other, other, AGENT, ATTESTATION_SCHEMA);
        let v = inspect_record(&a, AUTH);
        assert!(!v.matches_expected_envelope);
        assert!(v
            .reasons
            .iter()
            .any(|r| r.contains("not the Covenant authority")));
    }

    #[test]
    fn validator_must_mirror_authority() {
        // On-chain authority is Covenant's, but the payload's validator claims
        // someone else, a spoofed/copied payload. Must fail.
        let a = das_attestation(
            AUTH,
            "So11111111111111111111111111111111111111112",
            AGENT,
            ATTESTATION_SCHEMA,
        );
        let v = inspect_record(&a, AUTH);
        assert!(!v.matches_expected_envelope);
        assert!(v
            .reasons
            .iter()
            .any(|r| r.contains("validator field does not match")));
    }

    #[test]
    fn wrong_schema_fails() {
        let a = das_attestation(AUTH, AUTH, AGENT, "covenant.audit-root.appdata.v1");
        let v = inspect_record(&a, AUTH);
        assert!(!v.matches_expected_envelope);
        assert!(v.reasons.iter().any(|r| r.contains("schema is not")));
    }

    #[test]
    fn camelcase_payload_also_reads() {
        // Raw on-chain bytes are camelCase; a reader fetching the URI directly
        // (not via a re-casing indexer) must still verify.
        let a = json!({
            "id": "X",
            "external_plugins": [{
                "type": "AppData",
                "authority": { "address": AUTH },
                "adapterConfig": { "schema": "Json", "dataAuthority": { "address": AUTH } },
                "data": {
                    "type": ATTESTATION_TYPE, "schema": ATTESTATION_SCHEMA,
                    "subject": { "registry": SUBJECT_REGISTRY, "asset": AGENT },
                    "validator": AUTH,
                    "hashAlg": "sha256-merkle",
                    "responseHash": "7c375d0e0a749966541c7543b87b76f61fd4b64d41ff12473d68f3ff45caef26",
                    "tag": "audit",
                    "covenant": {
                        "releaseTarget": "covenant",
                        "releaseSubject": "witness-loop",
                        "releaseScope": "audit"
                    },
                    "recordedAt": 1781738307u64
                }
            }]
        });
        assert!(inspect_record(&a, AUTH).matches_expected_envelope);
    }

    #[test]
    fn forged_adapter_authority_does_not_verify() {
        // The forgery: a minter sets the adapter's `authority` to Covenant's key
        // (cosmetic, needs no signature) but the real write authority
        // (data_authority) is the attacker's, so the attacker controls the
        // payload. Reading data_authority (not `authority`) rejects it.
        let attacker = "So11111111111111111111111111111111111111112";
        let mut a = das_attestation(AUTH, AUTH, AGENT, ATTESTATION_SCHEMA);
        a["external_plugins"][0]["authority"]["address"] = json!(AUTH);
        a["external_plugins"][0]["adapter_config"]["data_authority"]["address"] = json!(attacker);
        a["external_plugins"][0]["data"]["validator"] = json!(AUTH);
        let v = inspect_record(&a, AUTH);
        assert!(
            !v.matches_expected_envelope,
            "a record whose reported data_authority differs must not match"
        );
        assert!(v
            .reasons
            .iter()
            .any(|r| r.contains("not the Covenant authority")));
    }

    #[test]
    fn no_appdata_does_not_match() {
        let v = inspect_record(&json!({ "id": "X", "external_plugins": [] }), AUTH);
        assert!(!v.matches_expected_envelope);
        assert!(v.reasons.iter().any(|r| r.contains("no AppData")));
    }

    #[test]
    fn malformed_root_fails() {
        let mut a = das_attestation(AUTH, AUTH, AGENT, ATTESTATION_SCHEMA);
        a["external_plugins"][0]["data"]["response_hash"] = json!("deadbeef");
        let v = inspect_record(&a, AUTH);
        assert!(!v.matches_expected_envelope);
        assert!(v
            .reasons
            .iter()
            .any(|r| r.contains("responseHash is not 64")));
    }

    #[test]
    fn complete_subject_and_covenant_envelope_are_required() {
        let mut a = das_attestation(AUTH, AUTH, AGENT, ATTESTATION_SCHEMA);
        let data = &mut a["external_plugins"][0]["data"];
        data["subject"] = json!({ "asset": AGENT });
        data.as_object_mut().unwrap().remove("tag");
        data.as_object_mut().unwrap().remove("covenant");

        let observation = inspect_record(&a, AUTH);

        assert!(!observation.matches_expected_envelope);
        for expected in [
            "subject.registry is not mpl-agent-014",
            "tag missing, empty, or unsafe",
            "covenant object missing",
            "covenant.releaseTarget missing, empty, or unsafe",
            "covenant.releaseSubject missing, empty, or unsafe",
            "covenant.releaseScope missing, empty, or unsafe",
        ] {
            assert!(
                observation.reasons.iter().any(|reason| reason == expected),
                "missing reason {expected:?}: {:?}",
                observation.reasons
            );
        }
    }

    #[test]
    fn subject_asset_timestamp_and_tag_scope_binding_are_required() {
        let mut a = das_attestation(AUTH, AUTH, AGENT, ATTESTATION_SCHEMA);
        let data = &mut a["external_plugins"][0]["data"];
        data["subject"] = json!({ "registry": SUBJECT_REGISTRY });
        data["tag"] = json!("different-scope");
        data.as_object_mut().unwrap().remove("recorded_at");

        let observation = inspect_record(&a, AUTH);

        assert!(!observation.matches_expected_envelope);
        for expected in [
            "subject.asset missing",
            "tag does not match covenant.releaseScope",
            "recordedAt missing",
        ] {
            assert!(
                observation.reasons.iter().any(|reason| reason == expected),
                "missing reason {expected:?}: {:?}",
                observation.reasons
            );
        }
    }

    #[test]
    fn recorded_at_must_fit_the_supported_unix_seconds_range() {
        let mut a = das_attestation(AUTH, AUTH, AGENT, ATTESTATION_SCHEMA);
        a["external_plugins"][0]["data"]["recorded_at"] = json!(MAX_UNIX_SECONDS + 1);

        let observation = inspect_record(&a, AUTH);

        assert!(!observation.matches_expected_envelope);
        assert!(observation
            .reasons
            .iter()
            .any(|reason| reason.contains("outside the supported Unix-seconds range")));
    }

    // --- agent-level (DAS-backed) ---

    struct StubDas {
        items: Vec<Value>,
    }
    #[async_trait::async_trait]
    impl DasClient for StubDas {
        async fn get_asset(&self, _id: &str) -> Result<Value, DasError> {
            Ok(Value::Null)
        }
        async fn get_asset_proof(&self, _id: &str) -> Result<Value, DasError> {
            Ok(Value::Null)
        }
        async fn get_assets_by_owner(
            &self,
            _o: &str,
            _l: u32,
            page: u32,
        ) -> Result<Value, DasError> {
            Ok(json!({ "items": if page == 1 { self.items.clone() } else { vec![] } }))
        }
        async fn search_assets(&self, _p: Value) -> Result<Value, DasError> {
            Ok(Value::Null)
        }
    }

    #[tokio::test]
    async fn agent_with_matching_record_is_reported() {
        let das = StubDas {
            items: vec![
                das_attestation(AUTH, AUTH, AGENT, ATTESTATION_SCHEMA),
                das_attestation(AUTH, AUTH, "someOtherAgent", ATTESTATION_SCHEMA), // different subject
            ],
        };
        let v = inspect_agent_records(&das, AGENT, AUTH).await.unwrap();
        assert!(v.has_matching_record);
        assert_eq!(v.record_count, 1, "only the subject-matched one counts");
        assert!(v.latest.is_some());
    }

    #[tokio::test]
    async fn agent_with_no_matching_record_is_reported() {
        let das = StubDas {
            items: vec![das_attestation(AUTH, AUTH, "elsewhere", ATTESTATION_SCHEMA)],
        };
        let v = inspect_agent_records(&das, AGENT, AUTH).await.unwrap();
        assert!(!v.has_matching_record);
        assert_eq!(v.record_count, 0);
        assert!(v.latest.is_none());
    }
}
