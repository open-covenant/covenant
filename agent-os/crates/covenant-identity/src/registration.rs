//! Dual-shaped agent registration document.
//!
//! [`AgentRegistration`] is one JSON object that is simultaneously a valid
//! A2A [`AgentCard`] and an ERC-8004 registration file, so an EVM /
//! ERC-8004 / A2A-aware client can discover and trust a Covenant agent
//! from a single document. It carries the union of both schemas' required
//! fields: an A2A client reads the A2A fields and ignores the rest, an
//! ERC-8004 client reads the ERC-8004 fields and ignores the rest. The
//! A2A AgentCard schema does not forbid extra properties, and every
//! ERC-8004 field except `supportedTrust` is mandatory, so the union
//! validates as both.
//!
//! The home registry is expressed in the CAIP-2 form EVM clients already
//! parse — `solana:<genesis-hash>:<registry>` — using the Solana
//! mainnet-beta genesis hash rather than a network name. The document is
//! signed with a detached JWS (RFC 7515, `alg=EdDSA`) over the RFC 8785
//! JCS canonicalization of the body, stored in the A2A `signatures` array
//! so the same signature is A2A-native.
//!
//! Field names are pinned to specific upstream spec commits
//! ([`ERC8004_SPEC_COMMIT`], [`A2A_SPEC_COMMIT`]) because the ERC-8004
//! draft has churned the trust field between `trustModels` and
//! `supportedTrust`; the fixture test asserts the current shape so a
//! future spec revision surfaces as a failing test, not a silently
//! unparseable card.
//!
//! Generation, signing, and verification are local only. Publishing to a
//! public `/.well-known` endpoint or an on-chain registry is out of scope
//! here (tracked under multichain-20).
//!
//! [`AgentCard`]: https://github.com/a2aproject/A2A

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

/// ERC-8004 registration file discriminator. Pinned to `ethereum/ERCs`
/// commit [`ERC8004_SPEC_COMMIT`].
pub const ERC8004_REGISTRATION_TYPE: &str =
    "https://eips.ethereum.org/EIPS/eip-8004#registration-v1";

/// `ethereum/ERCs` commit that `ERCS/erc-8004.md` field names are pinned
/// to. The trust field is `supportedTrust` at this commit.
pub const ERC8004_SPEC_COMMIT: &str = "503591a6e80e6e1affdd6403341e25269141f046";

/// `a2aproject/A2A` v0.3.0 tag commit that the AgentCard required-field
/// set is pinned to.
pub const A2A_SPEC_COMMIT: &str = "210f03d426e2f2fa92000e14ef0de3b7ba15aee5";

/// A2A protocol version advertised by the card and its A2A service entry.
pub const A2A_PROTOCOL_VERSION: &str = "0.3.0";

/// CAIP-2 chain id for Solana mainnet-beta: the namespace `solana` plus
/// the genesis hash truncated to its first 32 characters, per the
/// ChainAgnostic Solana namespace definition. Using this rather than a
/// network name is what lets an EVM/CAIP-aware client resolve the chain.
pub const SOLANA_MAINNET_CAIP2: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";

/// Default ERC-8004 trust models. Covenant advertises reputation trust;
/// `crypto-economic` and `tee-attestation` are added by later multichain
/// phases once the backing mechanisms exist.
pub const DEFAULT_SUPPORTED_TRUST: &[&str] = &["reputation"];

#[derive(Debug, thiserror::Error)]
pub enum RegistrationError {
    #[error("canonicalize: {0}")]
    Canonicalize(#[from] serde_json::Error),
    #[error("ed25519: {0}")]
    Signature(#[from] ed25519_dalek::SignatureError),
    #[error("document carries no signatures")]
    Unsigned,
    #[error("invalid base64url in signature envelope")]
    Base64,
}

/// Inputs a caller supplies alongside an agent identity to build a
/// registration document. Borrowed so callers can pass configuration
/// slices without allocating.
pub struct RegistrationParams<'a> {
    /// Human-readable agent name (A2A + ERC-8004 `name`).
    pub name: &'a str,
    /// Natural-language description (A2A + ERC-8004 `description`).
    pub description: &'a str,
    /// ERC-8004 `image` URL; kept for ERC-721 app compatibility.
    pub image: &'a str,
    /// A2A service base URL (AgentCard `url`), also used for the A2A
    /// service entry endpoint.
    pub url: &'a str,
    /// Agent software version (AgentCard `version`).
    pub version: &'a str,
    /// CAIP-2 chain reference of the home registry; defaults to
    /// [`SOLANA_MAINNET_CAIP2`].
    pub registry_caip2: &'a str,
    /// On-chain Covenant registry account/program address on
    /// `registry_caip2`. The `agentRegistry` field is
    /// `<registry_caip2>:<registry_address>`.
    pub registry_address: &'a str,
    /// ERC-8004 numeric `agentId` (the ERC-721 tokenId). Zero until the
    /// agent is registered on-chain (multichain-20).
    pub agent_id: u64,
    /// Whether the agent accepts x402 payments (ERC-8004 `x402Support`).
    pub x402_support: bool,
    /// ERC-8004 `active` flag.
    pub active: bool,
    /// ERC-8004 `supportedTrust` values.
    pub supported_trust: &'a [&'a str],
    /// A2A skills advertised by the card.
    pub skills: &'a [Skill],
    /// Endpoints appended after the derived A2A and DID service entries.
    pub extra_services: &'a [Service],
}

impl<'a> RegistrationParams<'a> {
    /// Params for a Solana-mainnet agent with sensible defaults: agentId
    /// unset, x402 enabled, active, reputation trust, no extra skills or
    /// services.
    pub fn solana_mainnet(
        name: &'a str,
        description: &'a str,
        image: &'a str,
        url: &'a str,
        version: &'a str,
        registry_address: &'a str,
    ) -> Self {
        Self {
            name,
            description,
            image,
            url,
            version,
            registry_caip2: SOLANA_MAINNET_CAIP2,
            registry_address,
            agent_id: 0,
            x402_support: true,
            active: true,
            supported_trust: DEFAULT_SUPPORTED_TRUST,
            skills: &[],
            extra_services: &[],
        }
    }
}

/// A2A `AgentCapabilities`. All fields optional; an empty object is a
/// valid capability set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub streaming: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub push_notifications: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_transition_history: Option<bool>,
}

/// A2A `AgentSkill`. `id`, `name`, `description`, and `tags` are required
/// by the pinned schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Skill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
}

/// One ERC-8004 `services[]` endpoint entry: `name`, `endpoint`, optional
/// `version`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Service {
    pub name: String,
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// One ERC-8004 `registrations[]` entry binding a numeric agentId to a
/// CAIP-2 registry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Registration {
    pub agent_id: u64,
    pub agent_registry: String,
}

/// An A2A `AgentCardSignature`: an RFC 7515 JWS with a detached payload
/// (the payload is the card body itself), so `protected` and `signature`
/// are the only stored members.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardSignature {
    /// base64url of the protected JWS header JSON.
    pub protected: String,
    /// base64url of the signature bytes.
    pub signature: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<serde_json::Value>,
}

/// The dual-shaped document. Field order is irrelevant to the signed
/// bytes — JCS sorts keys — so members are grouped by originating spec
/// for readability.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRegistration {
    /// ERC-8004 discriminator; harmless extra field to an A2A client.
    #[serde(rename = "type")]
    pub doc_type: String,

    // A2A AgentCard required fields.
    pub protocol_version: String,
    pub name: String,
    pub description: String,
    pub url: String,
    pub version: String,
    pub capabilities: Capabilities,
    pub default_input_modes: Vec<String>,
    pub default_output_modes: Vec<String>,
    pub skills: Vec<Skill>,

    // ERC-8004 required fields.
    pub image: String,
    pub services: Vec<Service>,
    pub x402_support: bool,
    pub active: bool,
    pub registrations: Vec<Registration>,

    // ERC-8004 optional trust field, and the A2A detached JWS signatures.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub supported_trust: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signatures: Vec<CardSignature>,
}

impl AgentRegistration {
    /// Build the unsigned document. `pubkey_base58` is the agent's Solana
    /// address (its ed25519 public key, base58-encoded); it becomes the
    /// `did:pkh` service subject.
    pub fn build(pubkey_base58: &str, params: &RegistrationParams) -> Self {
        let agent_registry = format!("{}:{}", params.registry_caip2, params.registry_address);
        let did = format!("did:pkh:{}:{}", params.registry_caip2, pubkey_base58);

        let mut services = vec![
            Service {
                name: "A2A".to_string(),
                endpoint: params.url.to_string(),
                version: Some(A2A_PROTOCOL_VERSION.to_string()),
            },
            Service {
                name: "DID".to_string(),
                endpoint: did,
                version: Some("v1".to_string()),
            },
        ];
        services.extend(params.extra_services.iter().cloned());

        Self {
            doc_type: ERC8004_REGISTRATION_TYPE.to_string(),
            protocol_version: A2A_PROTOCOL_VERSION.to_string(),
            name: params.name.to_string(),
            description: params.description.to_string(),
            url: params.url.to_string(),
            version: params.version.to_string(),
            capabilities: Capabilities::default(),
            default_input_modes: vec!["text/plain".to_string(), "application/json".to_string()],
            default_output_modes: vec!["text/plain".to_string(), "application/json".to_string()],
            skills: params.skills.to_vec(),
            image: params.image.to_string(),
            services,
            x402_support: params.x402_support,
            active: params.active,
            registrations: vec![Registration {
                agent_id: params.agent_id,
                agent_registry,
            }],
            supported_trust: params.supported_trust.iter().map(|s| s.to_string()).collect(),
            signatures: Vec::new(),
        }
    }

    /// RFC 8785 JCS canonical bytes of the body with the `signatures`
    /// array removed. This is the payload every signature is computed
    /// over, so it must be reproduced exactly on the verify side.
    pub fn signing_input_bytes(&self) -> Result<Vec<u8>, RegistrationError> {
        let mut body = self.clone();
        body.signatures.clear();
        Ok(serde_jcs::to_vec(&body)?)
    }

    /// Append a detached JWS (`alg=EdDSA`) over the JCS body.
    pub fn sign(&mut self, key: &SigningKey) -> Result<(), RegistrationError> {
        let protected = serde_jcs::to_vec(&serde_json::json!({ "alg": "EdDSA" }))?;
        let protected_b64 = base64url_encode(&protected);
        let payload_b64 = base64url_encode(&self.signing_input_bytes()?);
        let signing_input = format!("{protected_b64}.{payload_b64}");
        let sig = key.sign(signing_input.as_bytes());
        self.signatures.push(CardSignature {
            protected: protected_b64,
            signature: base64url_encode(&sig.to_bytes()),
            header: None,
        });
        Ok(())
    }

    /// Verify every attached signature against `key`. Errors if the
    /// document is unsigned or any signature fails.
    pub fn verify(&self, key: &VerifyingKey) -> Result<(), RegistrationError> {
        if self.signatures.is_empty() {
            return Err(RegistrationError::Unsigned);
        }
        let payload_b64 = base64url_encode(&self.signing_input_bytes()?);
        for entry in &self.signatures {
            let signing_input = format!("{}.{}", entry.protected, payload_b64);
            let sig_bytes = base64url_decode(&entry.signature)?;
            let sig = Signature::from_slice(&sig_bytes)?;
            key.verify_strict(signing_input.as_bytes(), &sig)?;
        }
        Ok(())
    }

    /// The document as a `serde_json::Value` in emission (non-canonical)
    /// form.
    pub fn to_value(&self) -> Result<serde_json::Value, RegistrationError> {
        Ok(serde_json::to_value(self)?)
    }
}

const B64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// URL-safe base64 without padding (RFC 4648 §5, no `=`), as required for
/// JWS members (RFC 7515 §2).
fn base64url_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64URL[((n >> 18) & 63) as usize] as char);
        out.push(B64URL[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64URL[((n >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(B64URL[(n & 63) as usize] as char);
        }
    }
    out
}

fn base64url_decode(input: &str) -> Result<Vec<u8>, RegistrationError> {
    fn sextet(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }
    let bytes = input.as_bytes();
    // A base64url group is 2, 3, or 4 chars; a remainder of 1 is impossible.
    if bytes.len() % 4 == 1 {
        return Err(RegistrationError::Base64);
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3 + 2);
    let mut acc = 0u32;
    let mut acc_bits = 0u32;
    for &c in bytes {
        let v = sextet(c).ok_or(RegistrationError::Base64)?;
        acc = (acc << 6) | v;
        acc_bits += 6;
        if acc_bits >= 8 {
            acc_bits -= 8;
            out.push((acc >> acc_bits) as u8);
            acc &= (1 << acc_bits) - 1;
        }
    }
    // Leftover padding bits (< 8) must be zero for a canonical encoding.
    if acc != 0 {
        return Err(RegistrationError::Base64);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn test_key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn test_pubkey_b58() -> String {
        bs58_like(&test_key().verifying_key().to_bytes())
    }

    // Minimal base58 (Bitcoin alphabet) so the test does not depend on the
    // bs58 crate; only used to check the DID subject equals the pubkey.
    fn bs58_like(input: &[u8]) -> String {
        const ALPHABET: &[u8; 58] =
            b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
        let mut digits: Vec<u8> = Vec::new();
        for &byte in input {
            let mut carry = byte as u32;
            for d in digits.iter_mut() {
                carry += (*d as u32) << 8;
                *d = (carry % 58) as u8;
                carry /= 58;
            }
            while carry > 0 {
                digits.push((carry % 58) as u8);
                carry /= 58;
            }
        }
        let mut out = String::new();
        for &byte in input {
            if byte == 0 {
                out.push('1');
            } else {
                break;
            }
        }
        for &d in digits.iter().rev() {
            out.push(ALPHABET[d as usize] as char);
        }
        out
    }

    fn sample_params<'a>() -> RegistrationParams<'a> {
        RegistrationParams::solana_mainnet(
            "covenant-agent",
            "A Covenant agent with governed, accountable execution.",
            "https://covenant.example/agent.png",
            "https://covenant.example/a2a",
            "0.1.0",
            "CovRegistry1111111111111111111111111111111",
        )
    }

    fn sample_doc() -> AgentRegistration {
        AgentRegistration::build(&test_pubkey_b58(), &sample_params())
    }

    #[test]
    fn document_carries_every_a2a_agentcard_required_field() {
        let v = sample_doc().to_value().unwrap();
        // The nine required members of the A2A AgentCard at A2A_SPEC_COMMIT.
        for field in [
            "capabilities",
            "defaultInputModes",
            "defaultOutputModes",
            "description",
            "name",
            "protocolVersion",
            "skills",
            "url",
            "version",
        ] {
            assert!(v.get(field).is_some(), "missing A2A required field {field}");
        }
        assert_eq!(v["protocolVersion"], Value::String(A2A_PROTOCOL_VERSION.into()));
    }

    #[test]
    fn document_carries_every_erc8004_required_field_and_registration_type() {
        let v = sample_doc().to_value().unwrap();
        for field in [
            "type",
            "name",
            "description",
            "image",
            "services",
            "x402Support",
            "active",
            "registrations",
        ] {
            assert!(v.get(field).is_some(), "missing ERC-8004 required field {field}");
        }
        // Omitting the registration-v1 type is expectedFailureMode 3.
        assert_eq!(v["type"], Value::String(ERC8004_REGISTRATION_TYPE.into()));
    }

    #[test]
    fn agent_registry_uses_solana_genesis_caip2_not_a_network_name() {
        let v = sample_doc().to_value().unwrap();
        let registry = v["registrations"][0]["agentRegistry"].as_str().unwrap();
        // expectedFailureMode 1: a network name instead of the genesis form.
        assert!(
            registry.starts_with("solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp:"),
            "agentRegistry must use the CAIP-2 genesis form, got {registry}"
        );
        assert!(!registry.contains("mainnet"), "must not use a network name");
    }

    #[test]
    fn trust_field_is_supported_trust_not_trust_models() {
        let v = sample_doc().to_value().unwrap();
        // expectedFailureMode 2: the draft churned trustModels -> supportedTrust.
        assert!(v.get("supportedTrust").is_some(), "must use supportedTrust");
        assert!(v.get("trustModels").is_none(), "must not use the stale trustModels name");
        assert_eq!(v["supportedTrust"], serde_json::json!(["reputation"]));
    }

    #[test]
    fn did_service_uses_did_pkh_solana_bound_to_the_agent_pubkey() {
        let doc = sample_doc();
        let did = doc
            .services
            .iter()
            .find(|s| s.name == "DID")
            .expect("a DID service entry")
            .endpoint
            .clone();
        let expected = format!(
            "did:pkh:solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp:{}",
            test_pubkey_b58()
        );
        assert_eq!(did, expected);
    }

    #[test]
    fn signed_document_verifies_with_the_signer_and_rejects_a_foreign_key() {
        let key = test_key();
        let mut doc = sample_doc();
        doc.sign(&key).unwrap();
        // expectedFailureMode 4: the card must be signed and verifiable.
        assert!(doc.verify(&key.verifying_key()).is_ok());

        let foreign = SigningKey::from_bytes(&[9u8; 32]);
        assert!(matches!(
            doc.verify(&foreign.verifying_key()),
            Err(RegistrationError::Signature(_))
        ));
    }

    #[test]
    fn unsigned_document_fails_verification() {
        let doc = sample_doc();
        assert!(matches!(
            doc.verify(&test_key().verifying_key()),
            Err(RegistrationError::Unsigned)
        ));
    }

    #[test]
    fn tampering_the_body_after_signing_breaks_the_signature() {
        let key = test_key();
        let mut doc = sample_doc();
        doc.sign(&key).unwrap();
        doc.name = "impostor".to_string();
        assert!(doc.verify(&key.verifying_key()).is_err());
    }

    #[test]
    fn jcs_body_is_deterministic_and_signature_independent() {
        let a = sample_doc();
        let mut b = sample_doc();
        b.sign(&test_key()).unwrap();
        // expectedFailureMode 4: JCS must be deterministic, and adding a
        // signature must not change the bytes it was computed over.
        assert_eq!(a.signing_input_bytes().unwrap(), b.signing_input_bytes().unwrap());
        // JCS emits lexicographically sorted keys.
        let jcs = String::from_utf8(a.signing_input_bytes().unwrap()).unwrap();
        let type_at = jcs.find("\"type\"").unwrap();
        let active_at = jcs.find("\"active\"").unwrap();
        assert!(active_at < type_at, "JCS keys must be sorted, so active precedes type");
    }

    #[test]
    fn signature_is_a2a_agentcardsignature_shaped_with_eddsa_header() {
        let mut doc = sample_doc();
        doc.sign(&test_key()).unwrap();
        let sig = &doc.signatures[0];
        let header: Value = serde_json::from_slice(&base64url_decode(&sig.protected).unwrap()).unwrap();
        assert_eq!(header["alg"], Value::String("EdDSA".into()));
        // 64-byte ed25519 signature round-trips through base64url.
        assert_eq!(base64url_decode(&sig.signature).unwrap().len(), 64);
    }

    #[test]
    fn document_round_trips_through_json() {
        let mut doc = sample_doc();
        doc.sign(&test_key()).unwrap();
        let json = serde_json::to_string(&doc).unwrap();
        let back: AgentRegistration = serde_json::from_str(&json).unwrap();
        assert_eq!(doc, back);
        assert!(back.verify(&test_key().verifying_key()).is_ok());
    }

    #[test]
    fn base64url_matches_rfc4648_vectors_and_round_trips() {
        // RFC 4648 §10 vectors under the URL-safe, no-pad alphabet.
        for (raw, encoded) in [
            (&b""[..], ""),
            (b"f", "Zg"),
            (b"fo", "Zm8"),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg"),
            (b"fooba", "Zm9vYmE"),
            (b"foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64url_encode(raw), encoded, "encode {raw:?}");
            assert_eq!(base64url_decode(encoded).unwrap(), raw, "decode {encoded}");
        }
        // URL-safe alphabet exercises - and _ (0xfb 0xff -> "-_8" with 2 pad bits).
        assert_eq!(base64url_encode(&[0xfb, 0xff]), "-_8");
        assert_eq!(base64url_decode("-_8").unwrap(), vec![0xfb, 0xff]);
        assert!(base64url_decode("A").is_err(), "length%4==1 is invalid");
        assert!(base64url_decode("**").is_err(), "non-alphabet bytes are invalid");
    }

    #[test]
    fn base64url_rejects_noncanonical_trailing_bits() {
        // A final base64url group carries unused low bits that a canonical
        // encoding leaves zero. Each twin below shares its length and alphabet
        // with a valid encoding, so neither the length nor the alphabet guard
        // fires — only the `acc != 0` check separates them. Accepting the
        // non-canonical form would let two distinct strings decode to the same
        // bytes, i.e. signature malleability in the JWS `protected`/`signature`
        // fields this decoder feeds.
        assert_eq!(base64url_decode("Zg").unwrap(), vec![0x66]);
        assert!(
            base64url_decode("Zh").is_err(),
            "2-char group with a nonzero trailing bit must be rejected",
        );
        assert_eq!(base64url_decode("Zm8").unwrap(), b"fo");
        assert!(
            base64url_decode("Zm9").is_err(),
            "3-char group with nonzero trailing bits must be rejected",
        );
    }

    #[test]
    fn spec_commits_are_pinned() {
        // The fixture: if an upstream spec revision changes the shape, this
        // pin plus the field-name assertions above force a deliberate update.
        assert_eq!(ERC8004_SPEC_COMMIT, "503591a6e80e6e1affdd6403341e25269141f046");
        assert_eq!(A2A_SPEC_COMMIT, "210f03d426e2f2fa92000e14ef0de3b7ba15aee5");
        assert_eq!(SOLANA_MAINNET_CAIP2, "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp");
        assert_eq!(
            ERC8004_REGISTRATION_TYPE,
            "https://eips.ethereum.org/EIPS/eip-8004#registration-v1"
        );
    }
}
