//! Wire types shared between the daemon and the `covenant-metaplex-signer`
//! sidecar.
//!
//! The daemon never holds the minting key or a solana-sdk dependency.
//! It builds one of these requests, pipes it as JSON to the sidecar's
//! stdin, and reads a [`SignerResponse`] back from stdout — the same
//! isolation pattern the x402 funding-key signer uses.

use serde::{Deserialize, Serialize};

/// Schema tag written alongside an attestation so DAS consumers can
/// decode the AppData payload. Bump the version if the field set changes.
pub const ATTESTATION_SCHEMA: &str = "covenant.audit-root.appdata.v1";

/// Validate a 32-byte audit/merkle root in its on-chain wire form: exactly
/// 64 lowercase ASCII hex characters.
///
/// Mirrors the SAP anchor's check so the two anchors agree on the
/// canonical form, and so a typo or truncated digest is rejected before
/// any subprocess spawn or on-chain write rather than being silently
/// inscribed. Used by both the daemon-side tool and the signer sidecar.
pub fn validate_root_hash_hex(s: &str) -> Result<(), String> {
    if s.len() != 64 {
        return Err(format!(
            "rootHashHex must be 64 hex characters, got {}",
            s.len()
        ));
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err("rootHashHex must be lowercase ASCII hex only".to_string());
    }
    Ok(())
}

/// Validate an ERC-8004 registration document URI before it is recorded
/// on-chain: `https://` or `ar://` only, no whitespace/control characters,
/// bounded length. Shared by the daemon-side tool and the signer sidecar
/// so a malformed URI is rejected before any subprocess spawn or send.
pub fn validate_registration_uri(s: &str) -> Result<(), String> {
    const MAX_LEN: usize = 200;
    if s.len() > MAX_LEN {
        return Err(format!(
            "registrationUri must be at most {MAX_LEN} bytes, got {}",
            s.len()
        ));
    }
    if !(s.starts_with("https://") || s.starts_with("ar://")) {
        return Err("registrationUri must start with https:// or ar://".to_string());
    }
    if s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("registrationUri must not contain whitespace or control characters".to_string());
    }
    Ok(())
}

/// JSON payload written into an MPL Core AppData plugin as a Covenant
/// attestation. Mirrors the daemon's audit-root attestation envelope:
/// identifiers and the 32-byte root only, never audit-log contents.
/// Stored as a JSON-schema AppData plugin, so DAS indexes it as JSON and
/// any wallet/explorer can read it without Covenant infrastructure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AttestationPayload {
    /// Always [`ATTESTATION_SCHEMA`]. Lets a reader know how to decode.
    pub schema: String,
    /// 32-byte audit/merkle root as lowercase hex (64 chars).
    pub root_hash_hex: String,
    pub release_target: String,
    pub release_subject: String,
    pub release_scope: String,
    pub recorded_at: u64,
}

impl AttestationPayload {
    pub fn new(
        root_hash_hex: impl Into<String>,
        release_target: impl Into<String>,
        release_subject: impl Into<String>,
        release_scope: impl Into<String>,
        recorded_at: u64,
    ) -> Self {
        Self {
            schema: ATTESTATION_SCHEMA.to_string(),
            root_hash_hex: root_hash_hex.into(),
            release_target: release_target.into(),
            release_subject: release_subject.into(),
            release_scope: release_scope.into(),
            recorded_at,
        }
    }
}

/// One request to the signer sidecar. Tagged JSON on stdin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum SignerRequest {
    /// Write an audit-root attestation into a Core asset's AppData plugin,
    /// with a Covenant-derived PDA as the data authority. `asset = None`
    /// mints a fresh attestation asset; `Some(_)` appends to an existing
    /// one.
    AttestAuditRoot {
        payload: AttestationPayload,
        #[serde(default)]
        asset: Option<String>,
        #[serde(default)]
        collection: Option<String>,
    },
    /// Bind the daemon's identity to an MPL Agent Identity record on a
    /// Core asset.
    RegisterIdentity {
        agent_label: String,
        agent_pubkey: String,
        #[serde(default)]
        asset: Option<String>,
        /// ERC-8004 registration document URI (`https://` or `ar://`).
        /// `None` falls back to the non-fetchable `covenant://agent/<pubkey>`
        /// identifier form.
        #[serde(default)]
        registration_uri: Option<String>,
    },
}

/// The signer's reply. JSON on stdout.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SignerResponse {
    /// Confirmed transaction signature.
    pub signature: String,
    /// The Core asset the attestation / identity landed on.
    pub asset: String,
    /// Cluster the transaction settled on.
    pub cluster: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attest_request_round_trips_tagged() {
        let req = SignerRequest::AttestAuditRoot {
            payload: AttestationPayload::new("a".repeat(64), "v0.1.0", "covenant", "audit", 1_700_000_000),
            asset: None,
            collection: Some("Coll1111111111111111111111111111111111111111".into()),
        };
        let wire = serde_json::to_value(&req).unwrap();
        assert_eq!(wire["action"], "attest-audit-root");
        assert_eq!(wire["payload"]["schema"], ATTESTATION_SCHEMA);
        let back: SignerRequest = serde_json::from_value(wire).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn root_hash_validation_matches_the_on_chain_wire_form() {
        validate_root_hash_hex(&"a".repeat(64)).expect("64 lowercase hex");
        validate_root_hash_hex(&"abcdef0123456789".repeat(4)).expect("mixed lowercase hex");
        assert!(validate_root_hash_hex("deadbeef").is_err(), "too short");
        assert!(validate_root_hash_hex(&"A".repeat(64)).is_err(), "uppercase");
        assert!(validate_root_hash_hex(&"g".repeat(64)).is_err(), "non-hex");
        assert!(validate_root_hash_hex(&"a".repeat(65)).is_err(), "too long");
    }

    #[test]
    fn identity_request_round_trips_tagged() {
        let req = SignerRequest::RegisterIdentity {
            agent_label: "agent@local".into(),
            agent_pubkey: "Agent11111111111111111111111111111111111111".into(),
            asset: None,
            registration_uri: Some("https://example.org/agent.json".into()),
        };
        let wire = serde_json::to_value(&req).unwrap();
        assert_eq!(wire["action"], "register-identity");
        let back: SignerRequest = serde_json::from_value(wire).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn registration_uri_validation_accepts_https_and_ar_only() {
        validate_registration_uri("https://opencovenant.org/agents/covenant.json")
            .expect("https URI");
        validate_registration_uri("ar://abc123").expect("arweave URI");
        assert!(validate_registration_uri("http://example.org/a.json").is_err(), "plain http");
        assert!(validate_registration_uri("covenant://agent/abc").is_err(), "covenant scheme is the default, not an override");
        assert!(validate_registration_uri("https://a.example/with space").is_err(), "whitespace");
        assert!(
            validate_registration_uri(&format!("https://a.example/{}", "x".repeat(200))).is_err(),
            "over length"
        );
    }

    #[test]
    fn identity_request_registration_uri_defaults_to_none() {
        let wire = serde_json::json!({
            "action": "register-identity",
            "agent_label": "agent@local",
            "agent_pubkey": "Agent11111111111111111111111111111111111111",
        });
        let back: SignerRequest = serde_json::from_value(wire).unwrap();
        match back {
            SignerRequest::RegisterIdentity {
                registration_uri, ..
            } => assert_eq!(registration_uri, None),
            other => panic!("unexpected request: {other:?}"),
        }
    }
}
