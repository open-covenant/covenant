use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    /// A signal record didn't carry the envelope fields the receipt binds to.
    #[error("signal: {0}")]
    Signal(String),
    #[error("decode: {0}")]
    Decode(String),
    /// Merkle inclusion was requested for a leaf that isn't in the tree.
    #[error("evidence: {0}")]
    Evidence(String),
}

pub type Result<T> = std::result::Result<T, Error>;
