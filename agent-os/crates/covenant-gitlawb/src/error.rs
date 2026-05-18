use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitlawbError {
    /// The bridge is disabled in config. Callers should treat this as a soft
    /// no-op, not a failure, it is the default state.
    #[error("gitlawb bridge is disabled")]
    Disabled,

    /// The node returned a non-success HTTP status.
    #[error("rpc: {status} {body}")]
    Rpc { status: u16, body: String },

    /// The input failed local validation before any network call fired.
    #[error("invalid input: {0}")]
    Invalid(String),

    /// A `did:key` could not be encoded or decoded.
    #[error("did: {0}")]
    Did(String),

    /// An HTTP Signature could not be constructed or verified.
    #[error("signature: {0}")]
    Signature(String),

    /// A signed ref-update certificate failed verification.
    #[error("cert: {0}")]
    Cert(String),

    /// Underlying HTTP transport failure.
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),

    /// URL parsing failure.
    #[error("url: {0}")]
    Url(#[from] url::ParseError),

    /// JSON serialization or deserialization failure.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    /// External attestation construction or verification failed.
    #[error("attest: {0}")]
    Attest(#[from] gitlawb_attest::AttestError),
}

pub type Result<T> = std::result::Result<T, GitlawbError>;
