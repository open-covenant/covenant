//! The second proof: a secp256k1 signature over an EIP-712 typed-data
//! digest, so an EVM contract can authenticate the attestation with one
//! `ecrecover` instead of parsing JSON-LD or running Ed25519.
//!
//! The signed struct exposes the fields an on-chain verifier actually
//! needs as native EIP-712 words — the 32-byte audit root, the credential
//! hash that commits to the rest of the document, and the validity
//! window as `uint256` seconds:
//!
//! ```text
//! CovenantAttestation(bytes32 auditRoot,bytes32 credentialHash,uint256 validFrom,uint256 validUntil)
//! ```
//!
//! The domain uses `salt` rather than `chainId`/`verifyingContract`
//! (mirroring the identity binding), so the same signature verifies on
//! Base, on a Solana precompile, and off-chain — an attestation
//! authorizes a statement, not a chain-specific transaction. Signing
//! reuses [`covenant_identity::Secp256k1IssuerKey`] from multichain-00.

use covenant_identity::Secp256k1IssuerKey;
use k256::ecdsa::{RecoveryId, Signature as EcdsaSignature, VerifyingKey};
use serde_json::{json, Value};
use sha3::{Digest, Keccak256};

use crate::{multibase, AttestationError};

pub const PROOF_TYPE: &str = "CovenantEip712Attestation";
const DOMAIN_NAME: &str = "Covenant Attestation";
const DOMAIN_VERSION: &str = "1";
const DOMAIN_SALT_PREIMAGE: &[u8] = b"covenant/attestation/vc/v1";
const EIP712_DOMAIN_TYPE: &[u8] = b"EIP712Domain(string name,string version,bytes32 salt)";
const ATTESTATION_TYPE: &[u8] =
    b"CovenantAttestation(bytes32 auditRoot,bytes32 credentialHash,uint256 validFrom,uint256 validUntil)";

/// The verified content of a `CovenantEip712Attestation` proof, returned
/// so the credential layer can bind it back to the document fields.
pub struct VerifiedEip712 {
    pub audit_root: [u8; 32],
    pub credential_hash: [u8; 32],
    pub valid_from: u64,
    pub valid_until: u64,
    pub issuer_address: [u8; 20],
}

fn keccak256(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Keccak256::digest(bytes));
    out
}

fn salt() -> [u8; 32] {
    keccak256(DOMAIN_SALT_PREIMAGE)
}

fn domain_separator() -> [u8; 32] {
    let mut buf = Vec::with_capacity(32 * 4);
    buf.extend_from_slice(&keccak256(EIP712_DOMAIN_TYPE));
    buf.extend_from_slice(&keccak256(DOMAIN_NAME.as_bytes()));
    buf.extend_from_slice(&keccak256(DOMAIN_VERSION.as_bytes()));
    buf.extend_from_slice(&salt());
    keccak256(&buf)
}

/// A `uint256` ABI word: 32-byte big-endian, left-padded.
fn uint256(value: u64) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[24..].copy_from_slice(&value.to_be_bytes());
    word
}

fn struct_hash(
    audit_root: &[u8; 32],
    credential_hash: &[u8; 32],
    valid_from: u64,
    valid_until: u64,
) -> [u8; 32] {
    let mut buf = Vec::with_capacity(32 * 5);
    buf.extend_from_slice(&keccak256(ATTESTATION_TYPE));
    buf.extend_from_slice(audit_root);
    buf.extend_from_slice(credential_hash);
    buf.extend_from_slice(&uint256(valid_from));
    buf.extend_from_slice(&uint256(valid_until));
    keccak256(&buf)
}

/// The EIP-712 signing digest: `keccak256(0x1901 ‖ domainSeparator ‖ structHash)`.
pub fn digest(
    audit_root: &[u8; 32],
    credential_hash: &[u8; 32],
    valid_from: u64,
    valid_until: u64,
) -> [u8; 32] {
    let mut buf = Vec::with_capacity(2 + 32 + 32);
    buf.push(0x19);
    buf.push(0x01);
    buf.extend_from_slice(&domain_separator());
    buf.extend_from_slice(&struct_hash(audit_root, credential_hash, valid_from, valid_until));
    keccak256(&buf)
}

fn did_pkh(address: &[u8; 20]) -> String {
    format!("did:pkh:eip155:1:0x{}", multibase::hex_encode(address))
}

/// Build the `CovenantEip712Attestation` proof. The embedded `eip712`
/// block is the full typed-data payload, so any EIP-712 verifier
/// (`eth_signTypedData` tooling or a Solidity verifier) can reconstruct
/// the digest without this crate.
pub fn create_proof(
    issuer: &Secp256k1IssuerKey,
    audit_root: &[u8; 32],
    credential_hash: &[u8; 32],
    valid_from: u64,
    valid_until: u64,
    created: &str,
) -> Value {
    let digest = digest(audit_root, credential_hash, valid_from, valid_until);
    let signature = issuer.sign_eip712_digest(&digest);
    let address = issuer.address();

    json!({
        "type": PROOF_TYPE,
        "created": created,
        "verificationMethod": did_pkh(&address),
        "proofPurpose": "assertionMethod",
        "proofValue": format!("0x{}", multibase::hex_encode(&signature)),
        "eip712": {
            "domain": {
                "name": DOMAIN_NAME,
                "version": DOMAIN_VERSION,
                "salt": format!("0x{}", multibase::hex_encode(&salt())),
            },
            "primaryType": "CovenantAttestation",
            "types": {
                "EIP712Domain": [
                    { "name": "name", "type": "string" },
                    { "name": "version", "type": "string" },
                    { "name": "salt", "type": "bytes32" },
                ],
                "CovenantAttestation": [
                    { "name": "auditRoot", "type": "bytes32" },
                    { "name": "credentialHash", "type": "bytes32" },
                    { "name": "validFrom", "type": "uint256" },
                    { "name": "validUntil", "type": "uint256" },
                ],
            },
            "message": {
                "auditRoot": format!("0x{}", multibase::hex_encode(audit_root)),
                "credentialHash": format!("0x{}", multibase::hex_encode(credential_hash)),
                "validFrom": valid_from,
                "validUntil": valid_until,
            },
        },
    })
}

/// Verify a `CovenantEip712Attestation` proof: reconstruct the digest
/// from the embedded message, recover the signer, and confirm it matches
/// the address named in `verificationMethod`. Returns the message values
/// for the credential layer to bind against the document.
pub fn verify_proof(proof: &Value) -> Result<VerifiedEip712, AttestationError> {
    let message = proof
        .get("eip712")
        .and_then(|e| e.get("message"))
        .ok_or_else(|| AttestationError::Field("eip712.message".into()))?;

    let audit_root = message_bytes32(message, "auditRoot")?;
    let credential_hash = message_bytes32(message, "credentialHash")?;
    let valid_from = message_u64(message, "validFrom")?;
    let valid_until = message_u64(message, "validUntil")?;

    let digest = digest(&audit_root, &credential_hash, valid_from, valid_until);

    let proof_value = proof
        .get("proofValue")
        .and_then(Value::as_str)
        .ok_or_else(|| AttestationError::Field("proofValue".into()))?;
    let recovered = recover_address(&digest, proof_value)?;

    let claimed = claimed_address(proof)?;
    if recovered != claimed {
        return Err(AttestationError::Secp256k1(
            "recovered signer does not match verificationMethod address".into(),
        ));
    }

    Ok(VerifiedEip712 {
        audit_root,
        credential_hash,
        valid_from,
        valid_until,
        issuer_address: recovered,
    })
}

fn message_bytes32(message: &Value, key: &str) -> Result<[u8; 32], AttestationError> {
    let raw = message
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| AttestationError::Field(format!("eip712.message.{key}")))?;
    let hex = raw
        .strip_prefix("0x")
        .ok_or_else(|| AttestationError::Field(format!("eip712.message.{key} missing 0x")))?;
    multibase::hex_decode_32(hex)
}

fn message_u64(message: &Value, key: &str) -> Result<u64, AttestationError> {
    message
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| AttestationError::Field(format!("eip712.message.{key}")))
}

fn claimed_address(proof: &Value) -> Result<[u8; 20], AttestationError> {
    let vm = proof
        .get("verificationMethod")
        .and_then(Value::as_str)
        .ok_or_else(|| AttestationError::Field("verificationMethod".into()))?;
    let token = vm
        .rsplit(':')
        .next()
        .ok_or_else(|| AttestationError::VerificationMethod(vm.to_string()))?;
    let hex = token
        .strip_prefix("0x")
        .ok_or_else(|| AttestationError::VerificationMethod(vm.to_string()))?;
    let bytes = multibase::hex_decode(hex)?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| AttestationError::VerificationMethod(vm.to_string()))
}

fn recover_address(digest: &[u8; 32], proof_value: &str) -> Result<[u8; 20], AttestationError> {
    let hex = proof_value
        .strip_prefix("0x")
        .ok_or_else(|| AttestationError::Secp256k1("proofValue missing 0x prefix".into()))?;
    let bytes = multibase::hex_decode(hex)?;
    if bytes.len() != 65 {
        return Err(AttestationError::Secp256k1(format!(
            "expected 65 signature bytes, got {}",
            bytes.len()
        )));
    }
    let recovery = bytes[64]
        .checked_sub(27)
        .and_then(RecoveryId::from_byte)
        .ok_or_else(|| AttestationError::Secp256k1(format!("bad recovery byte {}", bytes[64])))?;
    let signature = EcdsaSignature::from_slice(&bytes[..64])
        .map_err(|e| AttestationError::Secp256k1(e.to_string()))?;
    let key = VerifyingKey::recover_from_prehash(digest, &signature, recovery)
        .map_err(|e| AttestationError::Secp256k1(e.to_string()))?;
    Ok(eth_address(&key))
}

fn eth_address(vk: &VerifyingKey) -> [u8; 20] {
    let encoded = vk.to_encoded_point(false);
    let hash = keccak256(&encoded.as_bytes()[1..]);
    let mut address = [0u8; 20];
    address.copy_from_slice(&hash[12..]);
    address
}
