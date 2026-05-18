//! Outbound x402 client for Covenant agents.
//!
//! x402 is an open protocol for paid HTTP: a server responds with
//! `402 Payment Required` and a JSON body describing accepted
//! payment options; the client signs one option and retries with an
//! `x-payment` header. This crate is the local-side adapter the
//! Covenant daemon uses to make those calls on behalf of an agent.
//!
//! The crate stays narrow on purpose. It does not hold funding keys,
//! does not enforce per-day or total budgets, and does not record
//! receipts — those concerns live in the daemon (covenant-budget,
//! covenant-settlement, covenant-audit). What this crate does:
//!
//! - Parse a 402 challenge into typed [`PaymentRequirements`].
//! - Match requirements against a [`Capability`] the daemon supplies.
//! - Hand the chosen requirement to a [`Signer`] for the actual
//!   payment construction.
//! - Retry once with the resulting `x-payment` header and surface
//!   the paid response back to the caller.
//!
//! Skeleton crate: the real Solana signer lands once the daemon
//! owns funding-key access. Tests use a mock signer.

#![deny(unsafe_code)]

pub mod client;
pub mod orbit;
pub mod signer;
pub mod types;

#[cfg(feature = "solana")]
pub mod solana;

pub use client::Client;
pub use orbit::{Catalog, OrbitClient, Pagination, RegistryEntry, RegistryResponse};
pub use signer::{MockSigner, Signer};
pub use types::{Capability, PaymentRequirements};

#[cfg(feature = "solana")]
pub use solana::SolanaSigner;

/// Errors surfaced by the x402 client.
///
/// The surface is intentionally narrow so the daemon can map each
/// variant onto a stable audit-event kind without inspecting
/// upstream library errors.
#[derive(Debug, thiserror::Error)]
pub enum X402Error {
    /// The endpoint did not return 402 and did not return success.
    /// Holds the upstream status for diagnostics.
    #[error("unexpected status: {0}")]
    UnexpectedStatus(u16),
    /// The 402 body did not decode as an array of payment
    /// requirements.
    #[error("decode challenge: {0}")]
    DecodeChallenge(String),
    /// No payment requirement in the 402 response matched the
    /// supplied capability — wrong chain, wrong asset, or amount
    /// over the per-call cap.
    #[error("no payment requirement matched capability")]
    NoMatch,
    /// The signer failed to construct a payment payload.
    #[error("sign: {0}")]
    Sign(String),
    /// Underlying HTTP transport failure.
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
}

pub type Result<T> = std::result::Result<T, X402Error>;
