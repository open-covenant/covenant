use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    /// The recipient-standing read failed: the oracle's source was unreachable
    /// or returned something it couldn't turn into a standing.
    #[error("recipient oracle: {0}")]
    Oracle(String),
    #[error("decode: {0}")]
    Decode(String),
}

pub type Result<T> = std::result::Result<T, Error>;
