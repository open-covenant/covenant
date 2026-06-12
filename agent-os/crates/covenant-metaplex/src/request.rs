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
        return Err(
            "registrationUri must not contain whitespace or control characters".to_string(),
        );
    }
    Ok(())
}

/// Validate an attestation identifier string (`releaseTarget`,
/// `releaseSubject`, `releaseScope`) before it is inscribed into the
/// on-chain AppData JSON. The payload is rendered verbatim by DAS
/// consumers, wallets, and explorers, so these get the same length cap and
/// control-character guard as [`validate_registration_uri`]: a NUL/ESC/newline
/// inscribed on-chain is a render-injection risk, and an unbounded value
/// bloats the attestation. Whitespace is allowed — unlike a URI, a release
/// subject may legitimately contain spaces. Shared by the daemon-side tool
/// and the signer sidecar, which does not trust its stdin.
pub fn validate_attestation_field(name: &str, s: &str) -> Result<(), String> {
    const MAX_LEN: usize = 200;
    if s.len() > MAX_LEN {
        return Err(format!(
            "{name} must be at most {MAX_LEN} bytes, got {}",
            s.len()
        ));
    }
    if s.chars().any(|c| c.is_control()) {
        return Err(format!("{name} must not contain control characters"));
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
            payload: AttestationPayload::new(
                "a".repeat(64),
                "v0.1.0",
                "covenant",
                "audit",
                1_700_000_000,
            ),
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
        assert!(
            validate_root_hash_hex(&"A".repeat(64)).is_err(),
            "uppercase"
        );
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
        assert!(
            validate_registration_uri("http://example.org/a.json").is_err(),
            "plain http"
        );
        assert!(
            validate_registration_uri("covenant://agent/abc").is_err(),
            "covenant scheme is the default, not an override"
        );
        assert!(
            validate_registration_uri("https://a.example/with space").is_err(),
            "whitespace"
        );
        assert!(
            validate_registration_uri(&format!("https://a.example/{}", "x".repeat(200))).is_err(),
            "over length"
        );
    }

    #[test]
    fn attest_audit_root_omits_asset_and_collection_as_none() {
        // The daemon's minimal "mint a fresh attestation, no collection pin"
        // request is just {action, payload}: asset=None mints fresh, the
        // collection default means no group. Both rely on #[serde(default)];
        // without it this wire shape fails to deserialize in the sidecar and
        // every audit-root write breaks.
        let wire = serde_json::json!({
            "action": "attest-audit-root",
            "payload": {
                "schema": ATTESTATION_SCHEMA,
                "rootHashHex": "a".repeat(64),
                "releaseTarget": "v0.1.0",
                "releaseSubject": "covenant",
                "releaseScope": "audit",
                "recordedAt": 1_700_000_000u64,
            },
        });
        let back: SignerRequest = serde_json::from_value(wire).expect("minimal attest request");
        match back {
            SignerRequest::AttestAuditRoot {
                asset, collection, ..
            } => {
                assert_eq!(asset, None, "omitted asset mints a fresh attestation asset");
                assert_eq!(
                    collection, None,
                    "omitted collection means no collection pin"
                );
            }
            other => panic!("unexpected request: {other:?}"),
        }
    }

    #[test]
    fn attestation_payload_serializes_camelcase_appdata_keys() {
        // The payload is written verbatim into an on-chain MPL Core AppData
        // plugin and read back by DAS consumers as JSON, so the camelCase key
        // names are a public wire contract the symmetric round-trip test does
        // not pin (it only checks `schema`, identical in either case).
        let payload =
            AttestationPayload::new("a".repeat(64), "v0.1.0", "covenant", "audit", 1_700_000_000);
        let wire = serde_json::to_value(&payload).unwrap();
        let obj = wire
            .as_object()
            .expect("payload serializes to a JSON object");
        for key in [
            "schema",
            "rootHashHex",
            "releaseTarget",
            "releaseSubject",
            "releaseScope",
            "recordedAt",
        ] {
            assert!(obj.contains_key(key), "AppData payload must expose `{key}`");
        }
        for snake in [
            "root_hash_hex",
            "release_target",
            "release_subject",
            "release_scope",
            "recorded_at",
        ] {
            assert!(
                !obj.contains_key(snake),
                "snake_case `{snake}` would break DAS consumers"
            );
        }
        assert_eq!(obj.len(), 6, "exactly the six AppData fields, no extras");
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

    #[test]
    fn registration_uri_rejects_non_whitespace_control_chars() {
        // The whitespace test covers is_whitespace(); these are control but not
        // whitespace, exercising the is_control() half on its own. A NUL/ESC/DEL
        // inscribed into an on-chain URI is a render-injection risk.
        for uri in [
            "https://a.example/\u{0}",
            "https://a.example/\u{1b}",
            "https://a.example/\u{7f}",
        ] {
            assert!(
                validate_registration_uri(uri).is_err(),
                "control char in {uri:?} must be rejected"
            );
        }
    }

    #[test]
    fn registration_uri_length_cap_is_inclusive_at_200_bytes() {
        // "https://" is 8 bytes, so 8 + 192 = 200 is the accepted maximum.
        validate_registration_uri(&format!("https://{}", "a".repeat(192)))
            .expect("exactly 200 bytes is within the cap");
        assert!(
            validate_registration_uri(&format!("https://{}", "a".repeat(193))).is_err(),
            "201 bytes is over the cap"
        );
    }

    #[test]
    fn attestation_field_accepts_a_plain_label_including_spaces() {
        validate_attestation_field("releaseTarget", "v0.1.0").expect("version tag");
        validate_attestation_field("releaseSubject", "covenant control plane")
            .expect("a subject may contain spaces, unlike a URI");
        validate_attestation_field("releaseScope", &"a".repeat(200))
            .expect("exactly 200 bytes is within the cap");
    }

    #[test]
    fn attestation_field_rejects_control_characters() {
        // The payload is inscribed verbatim into the on-chain AppData JSON and
        // rendered by wallets/explorers/DAS, so a NUL/ESC/newline is the same
        // render-injection risk that registration_uri already rejects.
        for bad in ["audit\u{0}", "v0.1.0\u{1b}", "covenant\n", "scope\u{7f}"] {
            assert!(
                validate_attestation_field("releaseScope", bad).is_err(),
                "control char in {bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn attestation_field_rejects_over_length() {
        assert!(
            validate_attestation_field("releaseSubject", &"a".repeat(201)).is_err(),
            "201 bytes is over the cap"
        );
    }
}
