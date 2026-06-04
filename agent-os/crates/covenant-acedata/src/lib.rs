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
//! Payment is orthogonal: this profile uses AceData's Bearer billing
//! and never touches a funding key. Optional crypto pay-per-call is a
//! later phase that reuses the existing x402 gateway.

#![deny(unsafe_code)]

pub mod client;
pub mod config;
pub mod provenance;
pub mod tools;

pub use client::AceDataClient;
pub use config::AceDataConfig;
pub use provenance::Provenance;
pub use tools::{acedata_tools, PROVIDER};

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
