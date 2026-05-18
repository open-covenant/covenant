//! Error type for attestation operations.

use thiserror::Error;

/// Errors from building or verifying an attestation.
#[derive(Debug, Error)]
pub enum AttestError {
    /// Discriminator failed validation (empty, whitespace, etc.).
    #[error("attestation type: {0}")]
    Type(String),

    /// Attestation was signed against a different cert.
    #[error("cert hash mismatch")]
    CertHashMismatch,

    /// Ed25519 signature failed verification.
    #[error("signature: {0}")]
    Signature(String),

    /// DID could not be parsed.
    #[error("did: {0}")]
    Did(String),

    /// Payload structure check failed.
    #[error("payload: {0}")]
    Payload(String),

    /// No verifier registered for the type and policy required one.
    #[error("no verifier registered for type '{0}'")]
    UnknownType(String),

    /// JSON failure.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// Crate result alias.
pub type Result<T> = std::result::Result<T, AttestError>;
