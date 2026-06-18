//! Syra x402 provider profile for Covenant.
//!
//! Syra (api.syraa.fun) is an x402 v2 market-intelligence gateway. This
//! crate lets Covenant agents call its paid endpoints (signal, news,
//! sentiment, brain, smart-money) as MCP tools, paying per call in USDC
//! on the Solana rail.
//!
//! Payment reuses covenant-x402's [`covenant_x402::Signer`] (the
//! funding-key sidecar) and [`covenant_x402::PaymentRequirements`].
//! Syra's 402 is the official x402 v2 `accepts` shape; the matcher
//! deep-equals the chosen requirement against the payload's `accepted`,
//! so we echo it (see [`x402`]). The payment header is the standard
//! `x-payment`, and the retry hits the same URL.

pub mod config;
pub mod tools;
pub mod x402;

pub use config::SyraConfig;
pub use tools::{syra_specs, syra_tool, syra_tools, PaidRequest, PaidResponse, SyraExecutor};
pub use x402::{execute_paid, parse_challenge, PaidHttp};

/// Errors raised across the Syra profile.
#[derive(Debug, thiserror::Error)]
pub enum SyraError {
    #[error("challenge: {0}")]
    Challenge(String),
    #[error("not allowed: {0}")]
    NotAllowed(String),
    #[error("execute: {0}")]
    Execute(String),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
}

pub type Result<T> = std::result::Result<T, SyraError>;
