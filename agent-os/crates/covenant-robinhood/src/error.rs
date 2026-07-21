use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid credential: {0}")]
    Credential(String),
    #[error("http {status}: {body}")]
    Http { status: u16, body: String },
    #[error("transport: {0}")]
    Transport(String),
    #[error("decode: {0}")]
    Decode(String),
    #[error("blocked by policy: {0}")]
    Blocked(String),
}

pub type Result<T> = std::result::Result<T, Error>;
