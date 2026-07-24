//! Trust overlay for BlockRun's x402 API.
//!
//! BlockRun routes an agent's paid calls across LLMs, media, and data, settling
//! USDC over x402. It proves a payment cleared; it does not bind that payment to
//! what was requested, which model actually served the request, or what came
//! back. This crate captures the paid round-trip and emits a [`CallReceipt`]: a
//! hash-bound record tying the intent, the requested model, the model BlockRun
//! actually served, the input/output hashes, and the settlement together, so a
//! buyer gets a receipt of what was delivered, not just that money moved.
//!
//! It sits alongside BlockRun's payment flow and never intermediates it.

pub mod challenge;
pub mod client;
pub mod config;
pub mod receipt;
pub mod tools;

pub use challenge::{Accept, Challenge};
pub use client::{BlockRunClient, PaidCall};
pub use config::BlockRunConfig;
pub use receipt::{CallReceipt, PaymentInfo, RoutingClaim};
pub use tools::{blockrun_tools, CALL_TOOL, PROVIDER, VERIFY_TOOL};

#[derive(Debug, thiserror::Error)]
pub enum BlockRunError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("blockrun api {status}: {message}")]
    Api { status: u16, message: String },
    #[error("could not decode 402 challenge: {0}")]
    Challenge(String),
    #[error("payment signing failed: {0}")]
    Signer(String),
    #[error("unexpected: {0}")]
    Unexpected(String),
}

pub type Result<T> = std::result::Result<T, BlockRunError>;
