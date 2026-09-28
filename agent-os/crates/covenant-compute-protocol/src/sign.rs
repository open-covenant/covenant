//! Canonical signing for compute wire types.
//!
//! Every signed payload is serialized once with `serde_json` — struct
//! field order is declaration order, so this is deterministic without
//! needing a sort-keys-deep pass — then domain-separated and
//! ed25519-signed, mirroring `covenantd/src/lib.rs`'s `ATTEST_DOMAIN`
//! identity-attestation convention (`covenantd/src/lib.rs:5448-5487`).
//! The wire form carries the exact signed JSON string alongside the
//! signature (wrap, don't embed-then-strip), so a verifier never
//! re-serializes and risks canonicalization drift — the same shape as
//! `covenantd/src/escrow.rs`'s `CompletionProof`/`SignedCompletionProof`
//! (`covenantd/src/escrow.rs:81-119,205-216`).
//!
//! This uses the declaration-order serialization above rather than a
//! sort-keys-deep + hex-signature scheme: with no external signer to match
//! byte-for-byte, the simpler convention already proven elsewhere in this
//! codebase is preferred.

use covenant_identity::{verify_b58, IdentityError, LocalIdentity};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("signature: {0}")]
    BadSignature(#[from] IdentityError),
    #[error("{0}")]
    Invalid(String),
}

pub const JOB_ENVELOPE_DOMAIN: &[u8] = b"covenant.compute.job.v1\n";
pub const WORK_RECEIPT_DOMAIN: &[u8] = b"covenant.compute.receipt.v1\n";
pub const REGISTER_DOMAIN: &[u8] = b"covenant.compute.register.v1\n";
pub const HEARTBEAT_DOMAIN: &[u8] = b"covenant.compute.heartbeat.v1\n";
pub const ESCROW_HOLD_DOMAIN: &[u8] = b"covenant.compute.escrow_hold.v1\n";
pub const DISPUTE_DOMAIN: &[u8] = b"covenant.compute.dispute.v1\n";
pub const CANCEL_DOMAIN: &[u8] = b"covenant.compute.cancel.v1\n";
pub const LEASE_CLOSE_DOMAIN: &[u8] = b"covenant.compute.lease-close.v1\n";
pub const WITHDRAWAL_DOMAIN: &[u8] = b"covenant.compute.withdrawal.v1\n";
pub const UNBOND_DOMAIN: &[u8] = b"covenant.compute.unbond.v1\n";

pub(crate) fn to_canonical_json<T: Serialize>(value: &T) -> Result<String, ProtocolError> {
    Ok(serde_json::to_string(value)?)
}

/// Signs `domain || payload_json` and returns `(signature_b58, signer_pubkey_b58)`.
pub(crate) fn sign_domain(
    identity: &LocalIdentity,
    domain: &'static [u8],
    payload_json: &str,
) -> (String, String) {
    let mut signed = Vec::with_capacity(domain.len() + payload_json.len());
    signed.extend_from_slice(domain);
    signed.extend_from_slice(payload_json.as_bytes());
    let signature = identity.sign(&signed);
    let signature_b58 = bs58::encode(signature.to_bytes()).into_string();
    let signer_pubkey_b58 = bs58::encode(identity.pubkey_bytes()).into_string();
    (signature_b58, signer_pubkey_b58)
}

pub(crate) fn verify_domain(
    domain: &'static [u8],
    payload_json: &str,
    signature_b58: &str,
    signer_pubkey_b58: &str,
) -> Result<(), ProtocolError> {
    let mut signed = Vec::with_capacity(domain.len() + payload_json.len());
    signed.extend_from_slice(domain);
    signed.extend_from_slice(payload_json.as_bytes());
    verify_b58(signer_pubkey_b58, &signed, signature_b58)?;
    Ok(())
}
