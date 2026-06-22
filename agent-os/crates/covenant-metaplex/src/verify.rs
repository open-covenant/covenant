//! "Covenant Verified" — DAS-only verification of agent accountability.
//!
//! Anyone can run this against a public DAS endpoint with no Covenant
//! infrastructure in the path: it reads an MPL Core asset's AppData and
//! decides whether it is a valid Covenant audit-root attestation, and
//! whether a given agent identity is *accountable* (carries at least one).
//!
//! Trust anchor: MPL Core only lets the AppData `data_authority` write the
//! data, so authorship is a chain fact. A verifier checks that authority
//! against the expected Covenant attestation authority
//! ([`crate::config::COVENANT_ATTESTATION_AUTHORITY`]); the payload's
//! `validator` field must mirror it. The attestation must carry the
//! ERC-8004 validation `type`, the `covenant.audit-root.appdata.v2`
//! `schema`, a declared `hashAlg`, and a well-formed 64-hex `responseHash`.
//!
//! DAS indexers re-case the on-chain camelCase keys to snake_case (Helius
//! returns `response_hash`); every read here accepts both spellings.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::COVENANT_ATTESTATION_AUTHORITY;
use crate::das::{DasClient, DasError};
use crate::request::{
    validate_root_hash_hex, ATTESTATION_HASH_ALG, ATTESTATION_SCHEMA, ATTESTATION_TYPE,
};

/// Verdict for a single candidate attestation asset.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AttestationVerdict {
    /// The attestation asset id, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset: Option<String>,
    /// True iff every check below passed.
    pub verified: bool,
    /// The agent this attests to (`subject.asset`), when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject_asset: Option<String>,
    /// The on-chain AppData write authority.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authority: Option<String>,
    /// The attested 32-byte audit root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recorded_at: Option<u64>,
    /// Why it failed, empty when verified.
    pub reasons: Vec<String>,
}

/// Verdict for an agent identity: accountable iff it carries ≥1 verified
/// attestation authored by the expected authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentVerdict {
    pub agent: String,
    pub accountable: bool,
    pub attestation_count: usize,
    /// The most recent verified attestation, when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest: Option<AttestationVerdict>,
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

/// Verify one DAS asset as a Covenant attestation against `expected_authority`.
/// Pure: takes a DAS `getAsset` result (or one `items[]` entry). Never
/// network. Collects every failure reason rather than short-circuiting, so a
/// caller sees all of what's wrong.
pub fn verify_attestation(asset: &Value, expected_authority: &str) -> AttestationVerdict {
    let mut reasons = Vec::new();
    let asset_id = asset.get("id").and_then(Value::as_str).map(str::to_string);

    let Some(plugin) = app_data(asset) else {
        return AttestationVerdict {
            asset: asset_id,
            verified: false,
            subject_asset: None,
            authority: None,
            response_hash: None,
            recorded_at: None,
            reasons: vec!["no AppData external plugin on this asset".into()],
        };
    };
    let data = plugin.get("data").cloned().unwrap_or(Value::Null);
    let authority = plugin
        .get("authority")
        .and_then(|a| a.get("address"))
        .and_then(Value::as_str)
        .map(str::to_string);

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

    let subject_asset = data
        .get("subject")
        .and_then(|s| field(s, "asset", "asset"))
        .map(str::to_string);

    let recorded_at = data
        .get("recorded_at")
        .or_else(|| data.get("recordedAt"))
        .and_then(Value::as_u64);

    AttestationVerdict {
        asset: asset_id,
        verified: reasons.is_empty(),
        subject_asset,
        authority,
        response_hash,
        recorded_at,
        reasons,
    }
}

/// Is `agent_asset` accountable? Queries DAS for assets owned by the
/// expected authority, keeps the verified attestations whose `subject.asset`
/// is `agent_asset`, and reports the count + most recent. DAS-only; no key,
/// no Covenant infra.
pub async fn verify_agent(
    das: &dyn DasClient,
    agent_asset: &str,
    expected_authority: &str,
) -> Result<AgentVerdict, DasError> {
    let mut verified: Vec<AttestationVerdict> = Vec::new();
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
            let v = verify_attestation(item, expected_authority);
            if v.verified && v.subject_asset.as_deref() == Some(agent_asset) {
                verified.push(v);
            }
        }
        if items.len() < 1000 {
            break;
        }
    }

    let latest = verified
        .iter()
        .max_by_key(|v| v.recorded_at.unwrap_or(0))
        .cloned();
    Ok(AgentVerdict {
        agent: agent_asset.to_string(),
        accountable: !verified.is_empty(),
        attestation_count: verified.len(),
        latest,
    })
}

/// The production Covenant attestation authority.
pub fn default_authority() -> &'static str {
    COVENANT_ATTESTATION_AUTHORITY
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const AUTH: &str = COVENANT_ATTESTATION_AUTHORITY;
    const AGENT: &str = "4XtUrwvPWAzMGnsKenMpTMATXN3e2quJV11Jg2dab2dc";

    /// A DAS asset carrying a valid v2 attestation, snake_cased like Helius.
    fn das_attestation(authority: &str, validator: &str, subject: &str, schema: &str) -> Value {
        json!({
            "id": "7PEd79CG1hFUU9qeBnAKmyA77YWzckd572qsYdq3W3GH",
            "external_plugins": [{
                "type": "AppData",
                "authority": { "address": authority },
                "data": {
                    "type": ATTESTATION_TYPE,
                    "schema": schema,
                    "subject": { "registry": "mpl-agent-014", "asset": subject },
                    "validator": validator,
                    "hash_alg": "sha256-merkle",
                    "response_hash": "7c375d0e0a749966541c7543b87b76f61fd4b64d41ff12473d68f3ff45caef26",
                    "tag": "audit",
                    "recorded_at": 1781738307u64
                }
            }]
        })
    }

    #[test]
    fn valid_attestation_verifies() {
        let a = das_attestation(AUTH, AUTH, AGENT, ATTESTATION_SCHEMA);
        let v = verify_attestation(&a, AUTH);
        assert!(v.verified, "reasons: {:?}", v.reasons);
        assert_eq!(v.subject_asset.as_deref(), Some(AGENT));
        assert_eq!(v.authority.as_deref(), Some(AUTH));
        assert_eq!(v.response_hash.as_deref().map(|h| h.len()), Some(64));
    }

    #[test]
    fn wrong_authority_fails() {
        let other = "So11111111111111111111111111111111111111112";
        let a = das_attestation(other, other, AGENT, ATTESTATION_SCHEMA);
        let v = verify_attestation(&a, AUTH);
        assert!(!v.verified);
        assert!(v
            .reasons
            .iter()
            .any(|r| r.contains("not the Covenant authority")));
    }

    #[test]
    fn validator_must_mirror_authority() {
        // On-chain authority is Covenant's, but the payload's validator claims
        // someone else — a spoofed/copied payload. Must fail.
        let a = das_attestation(
            AUTH,
            "So11111111111111111111111111111111111111112",
            AGENT,
            ATTESTATION_SCHEMA,
        );
        let v = verify_attestation(&a, AUTH);
        assert!(!v.verified);
        assert!(v
            .reasons
            .iter()
            .any(|r| r.contains("validator field does not match")));
    }

    #[test]
    fn wrong_schema_fails() {
        let a = das_attestation(AUTH, AUTH, AGENT, "covenant.audit-root.appdata.v1");
        let v = verify_attestation(&a, AUTH);
        assert!(!v.verified);
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
                "data": {
                    "type": ATTESTATION_TYPE, "schema": ATTESTATION_SCHEMA,
                    "subject": { "asset": AGENT }, "validator": AUTH,
                    "hashAlg": "sha256-merkle",
                    "responseHash": "7c375d0e0a749966541c7543b87b76f61fd4b64d41ff12473d68f3ff45caef26",
                    "recordedAt": 1781738307u64
                }
            }]
        });
        assert!(verify_attestation(&a, AUTH).verified);
    }

    #[test]
    fn no_appdata_is_not_verified() {
        let v = verify_attestation(&json!({ "id": "X", "external_plugins": [] }), AUTH);
        assert!(!v.verified);
        assert!(v.reasons.iter().any(|r| r.contains("no AppData")));
    }

    #[test]
    fn malformed_root_fails() {
        let mut a = das_attestation(AUTH, AUTH, AGENT, ATTESTATION_SCHEMA);
        a["external_plugins"][0]["data"]["response_hash"] = json!("deadbeef");
        let v = verify_attestation(&a, AUTH);
        assert!(!v.verified);
        assert!(v
            .reasons
            .iter()
            .any(|r| r.contains("responseHash is not 64")));
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
    async fn agent_with_valid_attestation_is_accountable() {
        let das = StubDas {
            items: vec![
                das_attestation(AUTH, AUTH, AGENT, ATTESTATION_SCHEMA),
                das_attestation(AUTH, AUTH, "someOtherAgent", ATTESTATION_SCHEMA), // different subject
            ],
        };
        let v = verify_agent(&das, AGENT, AUTH).await.unwrap();
        assert!(v.accountable);
        assert_eq!(
            v.attestation_count, 1,
            "only the subject-matched one counts"
        );
        assert!(v.latest.is_some());
    }

    #[tokio::test]
    async fn agent_with_no_attestation_is_not_accountable() {
        let das = StubDas {
            items: vec![das_attestation(AUTH, AUTH, "elsewhere", ATTESTATION_SCHEMA)],
        };
        let v = verify_agent(&das, AGENT, AUTH).await.unwrap();
        assert!(!v.accountable);
        assert_eq!(v.attestation_count, 0);
        assert!(v.latest.is_none());
    }
}
