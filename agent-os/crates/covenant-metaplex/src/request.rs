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
pub const ATTESTATION_SCHEMA: &str = "covenant.audit-root.appdata.v2";

/// ERC-8004 validation discriminator. A generic reader can decode the
/// envelope as a validation attestation about an agent without any
/// Covenant-specific code; the MPL Agent validation registry is shaped
/// the same way (see [`crate::config::MPL_AGENT_VALIDATION_PROGRAM_ID`]).
pub const ATTESTATION_TYPE: &str = "https://eips.ethereum.org/EIPS/eip-8004#validation-v1";

/// Construction of [`AttestationPayload::response_hash`]. Declared
/// explicitly because ERC-8004 commitments are keccak256 `bytes32`; a
/// reader must know ours is a SHA-256 merkle root instead.
pub const ATTESTATION_HASH_ALG: &str = "sha256-merkle";

/// Registry the attestation subject lives in.
pub const SUBJECT_REGISTRY: &str = "mpl-agent-014";

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

/// Max bytes for any free-text string inscribed into the AppData payload.
const ONCHAIN_FIELD_MAX_LEN: usize = 200;

/// Control characters plus the bidirectional-override and zero-width /
/// invisible format characters of the Trojan-Source class (CVE-2021-42574):
/// these survive a `is_control()` check but can make an on-chain record
/// render as something other than its bytes. Rejected from every string
/// written on-chain. Kept byte-for-byte in sync with the SAP anchor.
fn is_unsafe_onchain_char(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{200B}'..='\u{200F}'   // ZWSP, ZWNJ, ZWJ, LRM, RLM
            | '\u{2028}'..='\u{2029}' // line + paragraph separators (forced breaks)
            | '\u{202A}'..='\u{202E}' // LRE, RLE, PDF, LRO, RLO
            | '\u{2060}'..='\u{2064}' // word joiner + invisible operators
            | '\u{2066}'..='\u{2069}' // LRI, RLI, FSI, PDI
            | '\u{FEFF}'              // BOM / zero-width no-break space
            | '\u{061C}',             // Arabic letter mark
        )
}

/// Validate a free-text identifier before it is inscribed into the AppData
/// JSON: bounded length, no control or bidi/zero-width formatting. `name`
/// is the field label for the error. Used for every non-pubkey, non-hash
/// string in the payload, on both the daemon and the signer.
pub fn validate_attestation_field(name: &str, s: &str) -> Result<(), String> {
    if s.len() > ONCHAIN_FIELD_MAX_LEN {
        return Err(format!(
            "{name} must be at most {ONCHAIN_FIELD_MAX_LEN} bytes, got {}",
            s.len()
        ));
    }
    if s.chars().any(is_unsafe_onchain_char) {
        return Err(format!(
            "{name} must not contain control characters or bidirectional/zero-width formatting"
        ));
    }
    Ok(())
}

/// Base58 charset + length sanity for a Solana pubkey written into the
/// AppData JSON, in this deliberately `solana-sdk`-free crate. Rejects a
/// poisoned/typo'd config value before it is inscribed; the signer
/// sidecar re-checks with a full pubkey parse before signing.
pub fn validate_onchain_pubkey(name: &str, s: &str) -> Result<(), String> {
    const BASE58: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    if !(32..=44).contains(&s.len()) {
        return Err(format!(
            "{name} must be a base58 pubkey of 32-44 chars, got {}",
            s.len()
        ));
    }
    if !s.bytes().all(|b| BASE58.contains(&b)) {
        return Err(format!(
            "{name} must be base58 (no 0, O, I, l or other non-base58 characters)"
        ));
    }
    Ok(())
}

/// Validate an ERC-8004 registration document URI before it is recorded
/// on-chain: `https://` or `ar://` only, no whitespace and no control or
/// bidi/zero-width formatting, bounded length. Shared by the daemon-side
/// tool and the signer so a malformed URI is rejected before any
/// subprocess spawn or send.
pub fn validate_registration_uri(s: &str) -> Result<(), String> {
    if s.len() > ONCHAIN_FIELD_MAX_LEN {
        return Err(format!(
            "registrationUri must be at most {ONCHAIN_FIELD_MAX_LEN} bytes, got {}",
            s.len()
        ));
    }
    if !(s.starts_with("https://") || s.starts_with("ar://")) {
        return Err("registrationUri must start with https:// or ar://".to_string());
    }
    if s.chars()
        .any(|c| c.is_whitespace() || is_unsafe_onchain_char(c))
    {
        return Err(
            "registrationUri must not contain whitespace, control characters, or bidirectional/zero-width formatting".to_string(),
        );
    }
    Ok(())
}

/// What the attestation is about: the agent's on-chain identity in the
/// MPL Agent (014) registry. Mirrors ERC-8004's `agentId` subject so a
/// reader resolves the attestation back to the agent it covers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AttestationSubject {
    /// Always [`SUBJECT_REGISTRY`].
    pub registry: String,
    /// The agent identity's MPL Core asset, when the daemon knows it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset: Option<String>,
    /// The 014 registry record (PDA) bound to that asset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registration: Option<String>,
    /// ERC-8004 numeric agent id, when one exists. Solana identities are
    /// keyed by asset/PDA, so this is usually `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

/// Covenant-domain identifiers naming what the root covers. Nested so the
/// outer envelope stays standard; never carries audit-log contents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CovenantRelease {
    pub release_target: String,
    pub release_subject: String,
    pub release_scope: String,
}

/// JSON payload written into an MPL Core AppData plugin as a Covenant
/// attestation, shaped as an ERC-8004 validation response: a `validator`
/// publishing a `responseHash` commitment about a `subject` agent. Stored
/// as a JSON-schema AppData plugin, so DAS indexes it as JSON and any
/// wallet/explorer can read it without Covenant infrastructure.
/// Identifiers and the 32-byte root only, never audit-log contents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AttestationPayload {
    /// Always [`ATTESTATION_TYPE`].
    #[serde(rename = "type")]
    pub r#type: String,
    /// Always [`ATTESTATION_SCHEMA`].
    pub schema: String,
    /// The agent this attests to.
    pub subject: AttestationSubject,
    /// The attesting authority. Stamped by the signer sidecar with its own
    /// data-authority pubkey, so it always reflects the key that signed
    /// rather than anything the daemon claimed; empty until then.
    pub validator: String,
    /// Always [`ATTESTATION_HASH_ALG`].
    pub hash_alg: String,
    /// 32-byte audit/merkle root as lowercase hex (64 chars): the ERC-8004
    /// `responseHash` commitment.
    pub response_hash: String,
    /// ERC-8004 categorization tag; mirrors [`CovenantRelease::release_scope`].
    pub tag: String,
    /// Covenant-domain identifiers.
    pub covenant: CovenantRelease,
    /// Unix seconds, stamped by the daemon.
    pub recorded_at: u64,
}

impl AttestationPayload {
    pub fn new(
        response_hash: impl Into<String>,
        release_target: impl Into<String>,
        release_subject: impl Into<String>,
        release_scope: impl Into<String>,
        recorded_at: u64,
    ) -> Self {
        let release_scope = release_scope.into();
        Self {
            r#type: ATTESTATION_TYPE.to_string(),
            schema: ATTESTATION_SCHEMA.to_string(),
            subject: AttestationSubject {
                registry: SUBJECT_REGISTRY.to_string(),
                asset: None,
                registration: None,
                agent_id: None,
            },
            validator: String::new(),
            hash_alg: ATTESTATION_HASH_ALG.to_string(),
            response_hash: response_hash.into(),
            tag: release_scope.clone(),
            covenant: CovenantRelease {
                release_target: release_target.into(),
                release_subject: release_subject.into(),
                release_scope,
            },
            recorded_at,
        }
    }

    /// Bind the attestation to the agent identity it covers: `asset` is the
    /// identity's MPL Core asset, `registration` its 014 registry PDA.
    pub fn with_subject(mut self, asset: Option<String>, registration: Option<String>) -> Self {
        self.subject.asset = asset;
        self.subject.registration = registration;
        self
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
        payload: Box<AttestationPayload>,
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
            payload: Box::new(AttestationPayload::new(
                "a".repeat(64),
                "v0.1.0",
                "covenant",
                "audit",
                1_700_000_000,
            )),
            asset: None,
            collection: Some("Coll1111111111111111111111111111111111111111".into()),
        };
        let wire = serde_json::to_value(&req).unwrap();
        assert_eq!(wire["action"], "attest-audit-root");
        let p = &wire["payload"];
        assert_eq!(p["schema"], ATTESTATION_SCHEMA);
        assert_eq!(p["type"], ATTESTATION_TYPE);
        assert_eq!(p["hashAlg"], ATTESTATION_HASH_ALG);
        assert_eq!(p["responseHash"], "a".repeat(64));
        assert_eq!(p["tag"], "audit");
        assert_eq!(p["subject"]["registry"], SUBJECT_REGISTRY);
        assert_eq!(p["covenant"]["releaseTarget"], "v0.1.0");
        let back: SignerRequest = serde_json::from_value(wire).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn with_subject_binds_agent_identity_and_omits_unset() {
        let p = AttestationPayload::new("b".repeat(64), "covenant", "witness-loop", "audit", 1)
            .with_subject(Some("Asset111".into()), Some("Pda111".into()));
        let wire = serde_json::to_value(&p).unwrap();
        assert_eq!(wire["subject"]["asset"], "Asset111");
        assert_eq!(wire["subject"]["registration"], "Pda111");
        assert!(
            wire["subject"].get("agentId").is_none(),
            "unset subject fields omitted"
        );
        let bare = AttestationPayload::new("c".repeat(64), "t", "s", "sc", 2);
        let bw = serde_json::to_value(&bare).unwrap();
        assert!(bw["subject"].get("asset").is_none());
        assert_eq!(
            bw["validator"], "",
            "validator empty until the signer stamps it"
        );
    }

    #[test]
    fn attestation_field_rejects_overlong_and_trojan_source_chars() {
        validate_attestation_field("tag", "audit").expect("plain ascii");
        validate_attestation_field("tag", "release-audit_v2").expect("dashes/underscores");
        assert!(
            validate_attestation_field("tag", &"x".repeat(201)).is_err(),
            "over 200 bytes"
        );
        // Trojan Source (CVE-2021-42574): pass is_control() but must be rejected.
        for bad in [
            "cove\u{202e}tnan", // RLO bidirectional override
            "covenant\u{200b}", // zero-width space
            "audit\u{2028}",    // line separator (forced break, not is_control)
            "tag\u{feff}",      // BOM
            "x\u{0007}",        // control (BEL)
        ] {
            assert!(
                validate_attestation_field("tag", bad).is_err(),
                "unsafe char in {bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn onchain_pubkey_validation_is_base58_and_length_bounded() {
        validate_onchain_pubkey(
            "subject.asset",
            "9sFJ95mZsBTGqTEBkcbmsx2V8RQiZ5iQACCLPLE61aWH",
        )
        .expect("real mainnet pubkey");
        assert!(
            validate_onchain_pubkey("x", "deadbeef").is_err(),
            "too short"
        );
        assert!(
            validate_onchain_pubkey("x", &"1".repeat(45)).is_err(),
            "too long"
        );
        assert!(
            validate_onchain_pubkey("x", &"0".repeat(40)).is_err(),
            "0 is not in the base58 alphabet"
        );
        assert!(
            validate_onchain_pubkey("x", "OIl0_______________________________________").is_err(),
            "non-base58 chars"
        );
    }

    #[test]
    fn registration_uri_rejects_bidi_and_zero_width_chars() {
        assert!(
            validate_registration_uri("https://a.example/\u{202e}evil.json").is_err(),
            "bidi override in URI must be rejected"
        );
        assert!(
            validate_registration_uri("https://a.example/\u{200b}.json").is_err(),
            "zero-width space in URI must be rejected"
        );
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
