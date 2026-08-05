use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    /// A binding input was not in the form the challenge is built from.
    #[error("binding: {0}")]
    Binding(String),
}

pub type Result<T> = std::result::Result<T, Error>;
