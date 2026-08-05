use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    /// An observation was not in a form the record can hold.
    #[error("observation: {0}")]
    Observation(String),
    /// The promise itself does not cohere.
    #[error("sla: {0}")]
    Sla(String),
    /// The bond or the slash policy puts nothing meaningful at risk.
    #[error("bond: {0}")]
    Bond(String),
    #[error("decode: {0}")]
    Decode(String),
}

pub type Result<T> = std::result::Result<T, Error>;
