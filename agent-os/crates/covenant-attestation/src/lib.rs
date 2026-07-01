//! Portable, dual-signed W3C Verifiable Credentials over a Covenant
//! audit root.
//!
//! The daemon anchors an audit-chain root on Solana through the
//! sap-bridge; that on-chain record is canonical but Solana-only. This
//! crate wraps the *same* root as a [W3C VCDM 2.0][vcdm] credential so
//! the statement is verifiable off-chain and, cheaply, on EVM — no
//! bridge required, because a signed statement carries its own trust.
//!
//! Every credential carries two proofs:
//!
//! - an **`eddsa-jcs-2022`** Data Integrity proof ([`eddsa`]) — the
//!   canonical, W3C-conformant signature; its `conformance` test module
//!   pins it to the specification's published test vector;
//! - a **`CovenantEip712Attestation`** proof ([`eip712`]) — a secp256k1
//!   signature over an EIP-712 digest an EVM contract can check with one
//!   `ecrecover`.
//!
//! [`AttestationInput::audit_root_hex`] is the exact 64-character
//! lowercase hex the sap-bridge anchors, and [`VerifiableCredential`]
//! re-exposes it, so the off-chain credential and the on-chain anchor
//! name the same 32 bytes.
//!
//! [vcdm]: https://www.w3.org/TR/vc-data-model-2.0/

mod bond;
mod eddsa;
mod eip712;
mod multibase;

use ed25519_dalek::SigningKey;
use serde_json::{json, Value};

use covenant_identity::Secp256k1IssuerKey;

pub use bond::{BaseNetwork, BondError, BondReceipt, SignedBondReceipt, BPS_DENOMINATOR};

/// Named error for every way an attestation can fail to issue or verify.
/// Verification variants say which check failed so a reviewer can tell a
/// forged proof from a malformed document.
#[derive(Debug, thiserror::Error)]
pub enum AttestationError {
    #[error("JSON canonicalization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("audit root must be 64 lowercase hex characters")]
    AuditRootFormat,
    #[error("invalid hex: {0}")]
    Hex(String),
    #[error("multibase decode failed: {0}")]
    Multibase(String),
    #[error("unexpected multicodec prefix")]
    Multicodec,
    #[error("bad verification method: {0}")]
    VerificationMethod(String),
    #[error("missing or malformed field: {0}")]
    Field(String),
    #[error("ed25519 (eddsa-jcs-2022) proof did not verify")]
    Ed25519,
    #[error("secp256k1 (EIP-712) proof did not verify: {0}")]
    Secp256k1(String),
    #[error("proof binding mismatch: {0}")]
    Binding(String),
    #[error("credential is missing its {0} proof")]
    MissingProof(&'static str),
}

/// The single privileged input a caller must scope: which audit root,
/// about which subject, valid for how long. Timestamps are Unix seconds
/// — rendered as RFC 3339 in the credential and carried as `uint256` in
/// the EIP-712 proof, so the caller never has to agree with us on a date
/// format.
pub struct AttestationInput {
    /// Credential `id`, e.g. a `urn:uuid:…`.
    pub credential_id: String,
    /// `credentialSubject.id` — the agent or task the root attests to.
    pub subject_id: String,
    /// The 64-character lowercase hex audit root, exactly as anchored.
    pub audit_root_hex: String,
    pub created_unix: u64,
    pub valid_from_unix: u64,
    pub valid_until_unix: u64,
    /// Optional Bitstring Status List revocation pointer.
    pub status: Option<StatusPointer>,
}

/// A `BitstringStatusListEntry`: the credential's pointer into a status
/// list so a revoked attestation stops verifying. The list itself is
/// hosted and signed separately; the credential only points at it.
pub struct StatusPointer {
    pub id: String,
    pub status_purpose: String,
    pub status_list_index: String,
    pub status_list_credential: String,
}

/// The issuer's two keys: Ed25519 for the canonical proof, secp256k1 for
/// the EVM-cheap one. The secp256k1 side is the multichain-00
/// [`Secp256k1IssuerKey`], whose secret is independent of the Ed25519
/// seed.
pub struct AttestationIssuer {
    ed25519: SigningKey,
    secp256k1: Secp256k1IssuerKey,
}

impl AttestationIssuer {
    pub fn new(ed25519: SigningKey, secp256k1: Secp256k1IssuerKey) -> Self {
        Self { ed25519, secp256k1 }
    }

    /// Fresh independent keys, for tests and ephemeral issuers. The two
    /// secrets draw from the same RNG but never share bytes.
    pub fn generate() -> Self {
        use rand::RngCore;
        let mut seed = [0u8; 32];
        rand::rng().fill_bytes(&mut seed);
        Self {
            ed25519: SigningKey::from_bytes(&seed),
            secp256k1: Secp256k1IssuerKey::generate(),
        }
    }

    /// The credential `issuer` DID (`did:key:z6Mk…`).
    pub fn issuer_did(&self) -> String {
        multibase::ed25519_did(&self.ed25519.verifying_key().to_bytes())
    }

    /// The 20-byte EVM address a chain-local verifier recovers from the
    /// EIP-712 proof.
    pub fn issuer_address(&self) -> [u8; 20] {
        self.secp256k1.address()
    }

    /// Issue a dual-signed attestation credential over `input`.
    pub fn issue(&self, input: &AttestationInput) -> Result<VerifiableCredential, AttestationError> {
        let audit_root = multibase::hex_decode_32(&input.audit_root_hex)
            .map_err(|_| AttestationError::AuditRootFormat)?;

        let pubkey = self.ed25519.verifying_key().to_bytes();
        let issuer_did = multibase::ed25519_did(&pubkey);
        let verification_method = multibase::ed25519_did_key(&pubkey);

        let mut credential = json!({
            "@context": ["https://www.w3.org/ns/credentials/v2"],
            "id": input.credential_id,
            "type": ["VerifiableCredential", "CovenantAuditAttestation"],
            "issuer": issuer_did,
            "validFrom": format_rfc3339(input.valid_from_unix),
            "validUntil": format_rfc3339(input.valid_until_unix),
            "credentialSubject": {
                "id": input.subject_id,
                "auditRoot": input.audit_root_hex,
                "anchor": {
                    "protocol": "solana-attestation-service",
                    "rootHashHex": input.audit_root_hex,
                },
            },
        });

        if let Some(status) = &input.status {
            credential["credentialStatus"] = json!({
                "id": status.id,
                "type": "BitstringStatusListEntry",
                "statusPurpose": status.status_purpose,
                "statusListIndex": status.status_list_index,
                "statusListCredential": status.status_list_credential,
            });
        }

        let credential_hash = canonical_sha256(&credential)?;
        let created = format_rfc3339(input.created_unix);

        let eddsa_proof =
            eddsa::create_proof(&credential, &self.ed25519, &verification_method, &created)?;
        let eip712_proof = eip712::create_proof(
            &self.secp256k1,
            &audit_root,
            &credential_hash,
            input.valid_from_unix,
            input.valid_until_unix,
            &created,
        );

        credential["proof"] = json!([eddsa_proof, eip712_proof]);
        Ok(VerifiableCredential { value: credential })
    }
}

/// A dual-signed attestation credential.
#[derive(Debug, Clone)]
pub struct VerifiableCredential {
    value: Value,
}

/// What a successful [`VerifiableCredential::verify`] establishes: both
/// proofs held, and these are the bound values a caller compares against
/// its own trust anchors (the known issuer address, the Solana-anchored
/// root, the current time).
#[derive(Debug, Clone)]
pub struct VerificationReport {
    /// The EVM address recovered from the EIP-712 proof.
    pub issuer_address: [u8; 20],
    /// The audit root both proofs commit to, as anchored hex.
    pub audit_root_hex: String,
    /// SHA-256 of the canonical (proof-stripped) document.
    pub credential_hash_hex: String,
    pub valid_from_unix: u64,
    pub valid_until_unix: u64,
}

impl VerifiableCredential {
    pub fn from_json(json: &str) -> Result<Self, AttestationError> {
        Ok(Self {
            value: serde_json::from_str(json)?,
        })
    }

    pub fn as_value(&self) -> &Value {
        &self.value
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.value).expect("a Value always serializes")
    }

    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(&self.value).expect("a Value always serializes")
    }

    /// The audit root the credential attests to, as anchored hex.
    pub fn audit_root_hex(&self) -> Result<&str, AttestationError> {
        self.value
            .get("credentialSubject")
            .and_then(|s| s.get("auditRoot"))
            .and_then(Value::as_str)
            .ok_or_else(|| AttestationError::Field("credentialSubject.auditRoot".into()))
    }

    /// SHA-256 of the JCS-canonical document with every proof removed —
    /// the stable hash a caller can anchor or dedupe on. Deterministic
    /// across re-serialization because it canonicalizes first.
    pub fn canonical_hash_hex(&self) -> Result<String, AttestationError> {
        let unsigned = self.unsigned()?;
        Ok(multibase::hex_encode(&canonical_sha256(&unsigned)?))
    }

    fn unsigned(&self) -> Result<Value, AttestationError> {
        let mut value = self.value.clone();
        value
            .as_object_mut()
            .ok_or_else(|| AttestationError::Field("credential".into()))?
            .remove("proof");
        Ok(value)
    }

    /// Verify both proofs and their binding to the document. Fails
    /// closed: either proof missing or failing, or any bound value
    /// disagreeing with the document, is an error — never a partial pass.
    pub fn verify(&self) -> Result<VerificationReport, AttestationError> {
        let proofs = self
            .value
            .get("proof")
            .and_then(Value::as_array)
            .ok_or(AttestationError::MissingProof("proof"))?;

        let unsigned = self.unsigned()?;
        let credential_hash = canonical_sha256(&unsigned)?;
        let audit_root_hex = self.audit_root_hex()?.to_string();
        let audit_root = multibase::hex_decode_32(&audit_root_hex)
            .map_err(|_| AttestationError::AuditRootFormat)?;
        let issuer = self
            .value
            .get("issuer")
            .and_then(Value::as_str)
            .ok_or_else(|| AttestationError::Field("issuer".into()))?;

        let mut ed25519_ok = false;
        let mut eip712 = None;
        for proof in proofs {
            match proof.get("type").and_then(Value::as_str) {
                Some(eddsa::PROOF_TYPE) => {
                    eddsa::verify_proof(&unsigned, proof)?;
                    assert_eddsa_signed_by_issuer(proof, issuer)?;
                    ed25519_ok = true;
                }
                Some(eip712::PROOF_TYPE) => {
                    eip712 = Some(eip712::verify_proof(proof)?);
                }
                _ => {}
            }
        }

        if !ed25519_ok {
            return Err(AttestationError::MissingProof("eddsa-jcs-2022"));
        }
        let eip712 = eip712.ok_or(AttestationError::MissingProof("secp256k1/EIP-712"))?;

        if eip712.audit_root != audit_root {
            return Err(AttestationError::Binding(
                "EIP-712 auditRoot != credentialSubject.auditRoot".into(),
            ));
        }
        if eip712.credential_hash != credential_hash {
            return Err(AttestationError::Binding(
                "EIP-712 credentialHash != canonical document hash".into(),
            ));
        }
        self.assert_time_field("validFrom", eip712.valid_from)?;
        self.assert_time_field("validUntil", eip712.valid_until)?;

        Ok(VerificationReport {
            issuer_address: eip712.issuer_address,
            audit_root_hex,
            credential_hash_hex: multibase::hex_encode(&credential_hash),
            valid_from_unix: eip712.valid_from,
            valid_until_unix: eip712.valid_until,
        })
    }

    /// The EIP-712 proof carries the validity window as `uint256`
    /// seconds; the document carries it as RFC 3339. Confirm they name
    /// the same instant so a caller can trust either surface.
    fn assert_time_field(&self, field: &str, unix: u64) -> Result<(), AttestationError> {
        let document = self
            .value
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| AttestationError::Field(field.to_string()))?;
        if document == format_rfc3339(unix) {
            Ok(())
        } else {
            Err(AttestationError::Binding(format!(
                "{field} disagrees between document and EIP-712 proof"
            )))
        }
    }
}

/// The canonical proof must be made by the key the credential names as
/// its `issuer`: the eddsa `verificationMethod`, minus its `#…` fragment,
/// has to be the issuer DID. Without this, a valid signature from *any*
/// Ed25519 key would satisfy the eddsa proof, leaving `issuer`
/// authenticated only by the secp256k1 side.
fn assert_eddsa_signed_by_issuer(proof: &Value, issuer: &str) -> Result<(), AttestationError> {
    let vm = proof
        .get("verificationMethod")
        .and_then(Value::as_str)
        .ok_or_else(|| AttestationError::Field("proof.verificationMethod".into()))?;
    let controller = vm.split('#').next().unwrap_or(vm);
    if controller == issuer {
        Ok(())
    } else {
        Err(AttestationError::Binding(
            "eddsa verificationMethod is not the credential issuer".into(),
        ))
    }
}

fn canonical_sha256(value: &Value) -> Result<[u8; 32], AttestationError> {
    use sha2::{Digest, Sha256};
    let mut out = [0u8; 32];
    out.copy_from_slice(&Sha256::digest(serde_jcs::to_vec(value)?));
    Ok(out)
}

/// Format Unix seconds as an RFC 3339 UTC timestamp,
/// `YYYY-MM-DDTHH:MM:SSZ`, using the civil-from-days algorithm — the
/// form VCDM 2.0 `validFrom`/`validUntil` expect, with no `chrono`
/// dependency.
fn format_rfc3339(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let secs = unix % 86_400;
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnant's `civil_from_days`: days since the Unix epoch to a
/// proleptic-Gregorian (year, month, day).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_identity::Secp256k1IssuerKey;

    fn fixed_issuer() -> AttestationIssuer {
        AttestationIssuer::new(
            SigningKey::from_bytes(&[7u8; 32]),
            Secp256k1IssuerKey::from_secret_bytes(&[9u8; 32]).unwrap(),
        )
    }

    fn sample_input() -> AttestationInput {
        AttestationInput {
            credential_id: "urn:uuid:11111111-2222-3333-4444-555555555555".into(),
            subject_id: "did:covenant:agent:research".into(),
            audit_root_hex: "a".repeat(64),
            created_unix: 1_700_000_000,
            valid_from_unix: 1_700_000_000,
            valid_until_unix: 1_800_000_000,
            status: Some(StatusPointer {
                id: "https://covenant.dev/status/1#7".into(),
                status_purpose: "revocation".into(),
                status_list_index: "7".into(),
                status_list_credential: "https://covenant.dev/status/1".into(),
            }),
        }
    }

    fn reparse_with(vc: &VerifiableCredential, mutate: impl FnOnce(&mut Value)) -> VerifiableCredential {
        let mut value = vc.as_value().clone();
        mutate(&mut value);
        VerifiableCredential::from_json(&serde_json::to_string(&value).unwrap()).unwrap()
    }

    #[test]
    fn issue_then_verify_round_trip() {
        let issuer = fixed_issuer();
        let vc = issuer.issue(&sample_input()).unwrap();
        let report = vc.verify().unwrap();

        assert_eq!(report.audit_root_hex, "a".repeat(64));
        assert_eq!(report.issuer_address, issuer.issuer_address());
        assert_eq!(report.valid_from_unix, 1_700_000_000);
        assert_eq!(report.valid_until_unix, 1_800_000_000);
        assert_eq!(vc.as_value()["proof"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn issuer_is_a_did_key_and_status_entry_is_well_formed() {
        let issuer = fixed_issuer();
        let vc = issuer.issue(&sample_input()).unwrap();
        let value = vc.as_value();

        assert_eq!(value["issuer"], issuer.issuer_did());
        assert!(issuer.issuer_did().starts_with("did:key:z6Mk"));
        assert_eq!(value["credentialStatus"]["type"], "BitstringStatusListEntry");
        assert_eq!(value["credentialStatus"]["statusPurpose"], "revocation");
        assert_eq!(value["@context"][0], "https://www.w3.org/ns/credentials/v2");
    }

    #[test]
    fn tampered_subject_breaks_the_eddsa_proof() {
        let issuer = fixed_issuer();
        let vc = issuer.issue(&sample_input()).unwrap();
        let forged = reparse_with(&vc, |v| {
            v["credentialSubject"]["id"] = json!("did:covenant:agent:attacker");
        });
        assert!(matches!(forged.verify(), Err(AttestationError::Ed25519)));
    }

    #[test]
    fn tampered_audit_root_is_rejected() {
        let issuer = fixed_issuer();
        let vc = issuer.issue(&sample_input()).unwrap();
        let forged = reparse_with(&vc, |v| {
            v["credentialSubject"]["auditRoot"] = json!("b".repeat(64));
        });
        assert!(forged.verify().is_err());
    }

    #[test]
    fn an_eddsa_proof_from_a_non_issuer_key_is_rejected() {
        let issuer = fixed_issuer();
        let vc = issuer.issue(&sample_input()).unwrap();

        // A different Ed25519 key re-signs the same document and points the
        // eddsa proof's verificationMethod at its own DID. The signature is
        // valid for that key, and the untouched EIP-712 proof still binds —
        // but the key is not the credential's issuer.
        let interloper = SigningKey::from_bytes(&[3u8; 32]);
        let vm = crate::multibase::ed25519_did_key(&interloper.verifying_key().to_bytes());
        let forged = reparse_with(&vc, |v| {
            let mut unsigned = v.clone();
            unsigned.as_object_mut().unwrap().remove("proof");
            let created = v["proof"][0]["created"].as_str().unwrap().to_string();
            v["proof"][0] =
                crate::eddsa::create_proof(&unsigned, &interloper, &vm, &created).unwrap();
        });

        assert!(matches!(forged.verify(), Err(AttestationError::Binding(_))));
    }

    #[test]
    fn a_missing_proof_fails_closed() {
        let issuer = fixed_issuer();
        let vc = issuer.issue(&sample_input()).unwrap();
        // Drop the EIP-712 proof, keep only the eddsa one.
        let stripped = reparse_with(&vc, |v| {
            v["proof"].as_array_mut().unwrap().truncate(1);
        });
        assert!(matches!(
            stripped.verify(),
            Err(AttestationError::MissingProof("secp256k1/EIP-712"))
        ));
    }

    #[test]
    fn a_corrupt_secp256k1_signature_is_rejected() {
        let issuer = fixed_issuer();
        let vc = issuer.issue(&sample_input()).unwrap();
        let forged = reparse_with(&vc, |v| {
            for proof in v["proof"].as_array_mut().unwrap() {
                if proof["type"] == crate::eip712::PROOF_TYPE {
                    proof["proofValue"] = json!(format!("0x{}", "11".repeat(65)));
                }
            }
        });
        assert!(matches!(forged.verify(), Err(AttestationError::Secp256k1(_))));
    }

    #[test]
    fn a_bad_audit_root_is_refused_at_issue() {
        let issuer = fixed_issuer();
        let mut input = sample_input();
        input.audit_root_hex = "A".repeat(64); // uppercase is not the anchored form
        assert!(matches!(
            issuer.issue(&input),
            Err(AttestationError::AuditRootFormat)
        ));
        input.audit_root_hex = "a".repeat(63); // wrong length
        assert!(matches!(
            issuer.issue(&input),
            Err(AttestationError::AuditRootFormat)
        ));
    }

    #[test]
    fn issuance_is_deterministic() {
        let issuer = fixed_issuer();
        let a = issuer.issue(&sample_input()).unwrap();
        let b = issuer.issue(&sample_input()).unwrap();
        // Ed25519 and RFC-6979 ECDSA are both deterministic, so a fixed
        // issuer over fixed input produces byte-identical credentials.
        assert_eq!(a.to_json(), b.to_json());
    }

    #[test]
    fn canonical_hash_survives_reserialization() {
        let issuer = fixed_issuer();
        let vc = issuer.issue(&sample_input()).unwrap();
        let before = vc.canonical_hash_hex().unwrap();

        let round_tripped = VerifiableCredential::from_json(&vc.to_json()).unwrap();
        assert_eq!(round_tripped.canonical_hash_hex().unwrap(), before);
        assert!(round_tripped.verify().is_ok());
    }

    #[test]
    fn validity_window_is_covered() {
        let issuer = fixed_issuer();
        let vc = issuer.issue(&sample_input()).unwrap();
        let forged = reparse_with(&vc, |v| {
            v["validUntil"] = json!("2099-01-01T00:00:00Z");
        });
        assert!(forged.verify().is_err());
    }

    #[test]
    fn rfc3339_matches_known_instants() {
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_rfc3339(1_672_531_200), "2023-01-01T00:00:00Z");
        assert_eq!(format_rfc3339(1_677_281_798), "2023-02-24T23:36:38Z");
    }
}
