//! The `eddsa-jcs-2022` Data Integrity cryptosuite (W3C VC-DI-EdDSA),
//! the canonical proof on every Covenant attestation.
//!
//! Per §3.3 of the specification the proof binds two SHA-256 digests:
//! `hashData = SHA256(JCS(proofConfig)) ‖ SHA256(JCS(document))`, with
//! the proof configuration hashed *first*. The proof configuration is
//! the proof options without `proofValue`, and — matching the spec's own
//! signed example — it carries the document `@context`. The 64-byte
//! `hashData` is signed with pure Ed25519 and the signature is emitted
//! as a base58-btc multibase `proofValue`.
//!
//! The `conformance` test module below replays the specification's
//! published key and document and asserts the exact `proofValue`, so this
//! is a spec match, not a self-consistent round trip.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::{multibase, AttestationError};

pub const PROOF_TYPE: &str = "DataIntegrityProof";
pub const CRYPTOSUITE: &str = "eddsa-jcs-2022";
pub const PROOF_PURPOSE: &str = "assertionMethod";

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Sha256::digest(bytes));
    out
}

/// The proof configuration (§3.3.5): the proof options carrying the
/// document `@context`, before `proofValue` exists.
fn proof_config(context: &Value, verification_method: &str, created: &str) -> Value {
    json!({
        "@context": context,
        "type": PROOF_TYPE,
        "cryptosuite": CRYPTOSUITE,
        "created": created,
        "verificationMethod": verification_method,
        "proofPurpose": PROOF_PURPOSE,
    })
}

/// `hashData` = `SHA256(JCS(proofConfig)) ‖ SHA256(JCS(document))`.
fn hash_data(document: &Value, config: &Value) -> Result<[u8; 64], AttestationError> {
    let config_hash = sha256(&serde_jcs::to_vec(config)?);
    let document_hash = sha256(&serde_jcs::to_vec(document)?);
    let mut out = [0u8; 64];
    out[..32].copy_from_slice(&config_hash);
    out[32..].copy_from_slice(&document_hash);
    Ok(out)
}

/// Sign `document` (which must not contain a `proof`) and return the
/// completed `eddsa-jcs-2022` proof object.
pub fn create_proof(
    document: &Value,
    signing_key: &SigningKey,
    verification_method: &str,
    created: &str,
) -> Result<Value, AttestationError> {
    let context = document
        .get("@context")
        .cloned()
        .ok_or_else(|| AttestationError::Field("@context".into()))?;
    let config = proof_config(&context, verification_method, created);
    let hash_data = hash_data(document, &config)?;
    let signature = signing_key.sign(&hash_data);
    let proof_value = multibase::base58btc(&signature.to_bytes());

    let mut proof = config
        .as_object()
        .expect("proof_config builds an object")
        .clone();
    proof.insert("proofValue".into(), Value::String(proof_value));
    Ok(Value::Object(proof))
}

/// Verify one `eddsa-jcs-2022` proof against `document` (the credential
/// with every `proof` removed). Recovers the public key from the proof's
/// own `verificationMethod`, so no external DID resolution is needed.
pub fn verify_proof(document: &Value, proof: &Value) -> Result<(), AttestationError> {
    let object = proof
        .as_object()
        .ok_or_else(|| AttestationError::Field("proof".into()))?;

    require_str(object, "type", PROOF_TYPE)?;
    require_str(object, "cryptosuite", CRYPTOSUITE)?;

    let verification_method = str_field(object, "verificationMethod")?;
    let pubkey = multibase::ed25519_public_key_from_verification_method(verification_method)?;
    let verifying_key = VerifyingKey::from_bytes(&pubkey).map_err(|_| AttestationError::Ed25519)?;

    let proof_value = str_field(object, "proofValue")?;
    let signature_bytes = multibase::base58btc_signature(proof_value)?;
    let signature =
        Signature::from_slice(&signature_bytes).map_err(|_| AttestationError::Ed25519)?;

    let mut config = object.clone();
    config.remove("proofValue");
    let hash_data = hash_data(document, &Value::Object(config))?;

    verifying_key
        .verify(&hash_data, &signature)
        .map_err(|_| AttestationError::Ed25519)
}

fn str_field<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, AttestationError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| AttestationError::Field(key.to_string()))
}

fn require_str(
    object: &Map<String, Value>,
    key: &str,
    expected: &str,
) -> Result<(), AttestationError> {
    if str_field(object, key)? == expected {
        Ok(())
    } else {
        Err(AttestationError::Field(format!("{key} != {expected}")))
    }
}

#[cfg(test)]
mod conformance {
    //! Replays the representative example from the W3C VC-DI-EdDSA
    //! specification (§ eddsa-jcs-2022) and asserts the published
    //! `proofValue` exactly. This is the difference between conforming to
    //! the standard and being self-consistent: a round trip signs and
    //! verifies with our own code and passes regardless of whether the
    //! canonicalization, hash order, or proof-config shape match the
    //! spec. Reproducing the spec's own signature over the spec's own key
    //! cannot.
    use super::*;
    use ed25519_dalek::SigningKey;
    use serde_json::json;

    const SECRET_KEY_MULTIBASE: &str = "z3u2en7t5LR2WtQH5PfFqMqwVHBeXouLzo6haApm8XHqvjxq";
    const PUBLIC_KEY_MULTIBASE: &str = "z6MkrJVnaZkeFzdQyMZu1cgjg7k1pZZ6pvBQ7XJPt4swbTQ2";
    const VERIFICATION_METHOD: &str = "did:key:z6MkrJVnaZkeFzdQyMZu1cgjg7k1pZZ6pvBQ7XJPt4swbTQ2#z6MkrJVnaZkeFzdQyMZu1cgjg7k1pZZ6pvBQ7XJPt4swbTQ2";
    const CREATED: &str = "2023-02-24T23:36:38Z";
    const EXPECTED_PROOF_VALUE: &str =
        "z2HnFSSPPBzR36zdDgK8PbEHeXbR56YF24jwMpt3R1eHXQzJDMWS93FCzpvJpwTWd3GAVFuUfjoJdcnTMuVor51aX";

    fn unsigned_credential() -> Value {
        json!({
            "@context": [
                "https://www.w3.org/ns/credentials/v2",
                "https://www.w3.org/ns/credentials/examples/v2"
            ],
            "id": "urn:uuid:58172aac-d8ba-11ed-83dd-0b3aef56cc33",
            "type": ["VerifiableCredential", "AlumniCredential"],
            "name": "Alumni Credential",
            "description": "A minimum viable example of an Alumni Credential.",
            "issuer": "https://vc.example/issuers/5678",
            "validFrom": "2023-01-01T00:00:00Z",
            "credentialSubject": {
                "id": "did:example:abcdefgh",
                "alumniOf": "The School of Examples"
            }
        })
    }

    #[test]
    fn matches_w3c_eddsa_jcs_2022_proof_value() {
        let seed = multibase::ed25519_seed_from_secret_multibase(SECRET_KEY_MULTIBASE).unwrap();
        let signing_key = SigningKey::from_bytes(&seed);

        // The secret decodes to the spec's key pair.
        let derived =
            multibase::ed25519_public_key_multibase(&signing_key.verifying_key().to_bytes());
        assert_eq!(derived, PUBLIC_KEY_MULTIBASE);

        let proof = create_proof(
            &unsigned_credential(),
            &signing_key,
            VERIFICATION_METHOD,
            CREATED,
        )
        .unwrap();
        assert_eq!(
            proof["proofValue"].as_str().unwrap(),
            EXPECTED_PROOF_VALUE,
            "eddsa-jcs-2022 proofValue must byte-match the W3C test vector"
        );

        verify_proof(&unsigned_credential(), &proof).unwrap();
    }

    #[test]
    fn tampered_document_fails_verification() {
        let seed = multibase::ed25519_seed_from_secret_multibase(SECRET_KEY_MULTIBASE).unwrap();
        let signing_key = SigningKey::from_bytes(&seed);
        let proof = create_proof(
            &unsigned_credential(),
            &signing_key,
            VERIFICATION_METHOD,
            CREATED,
        )
        .unwrap();

        let mut tampered = unsigned_credential();
        tampered["credentialSubject"]["alumniOf"] = json!("A Different School");
        assert!(verify_proof(&tampered, &proof).is_err());
    }
}
