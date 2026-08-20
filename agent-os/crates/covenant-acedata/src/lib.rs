//! AceData generative-capability provider for Covenant.
//!
//! AceData Cloud is a unified gateway to many generative services
//! (image, music, search, and more). This crate exposes a curated
//! subset as native [`covenant_mcp::Tool`]s over the standard
//! Bearer-token API — it is a *provider profile*, not a second runtime.
//!
//! The value Covenant adds is not the call; it is the governance around
//! it. Tools register into the daemon's [`covenant_mcp::ToolRegistry`],
//! so every AceData call is subject to the same capability checks and
//! audit trail as any other tool. On top of that, each call returns a
//! [`provenance::Provenance`] record — model, prompt hash, output hash,
//! asset references, provider task id — the anchor a verifiable
//! generation is later proven against.
//!
//! Payment has two modes (see [`client`]): AceData's Bearer billing with
//! an API key, or — with no key at all — keyless crypto pay-per-call,
//! where each call walks AceData's x402 402 challenge and pays USDC on
//! Solana through the funding-key signer ([`x402`]). The tools are
//! identical either way; the daemon picks the mode from the operator's
//! config.

#![deny(unsafe_code)]

pub mod client;
pub mod config;
pub mod provenance;
pub mod tools;
pub mod x402;

pub use client::AceDataClient;
pub use config::AceDataConfig;
pub use provenance::{Cost, Provenance, CREDIT_USD_RATE};
pub use tools::{acedata_tools, IMAGE_TOOL, MUSIC_TOOL, PROVIDER, SEARCH_TOOL};
pub use x402::X402Payer;

/// Errors surfaced by the AceData client.
#[derive(Debug, thiserror::Error)]
pub enum AceDataError {
    /// Transport-level failure reaching the API.
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    /// AceData answered with its `{success:false, error:{...}}` envelope.
    #[error("api error [{code}]: {message}")]
    Api { code: String, message: String },
    /// A response that did not match any shape we know how to read.
    #[error("unexpected response: {0}")]
    Unexpected(String),
}

pub type Result<T> = std::result::Result<T, AceDataError>;
