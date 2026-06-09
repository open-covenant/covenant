//! Xona Agent as an x402 provider profile.
//!
//! Xona Agent publishes a catalog of paid generation and market-data
//! endpoints (image, audio, token intelligence) as x402 HTTP: each
//! endpoint answers a 402 challenge and settles in USDC on Solana. This
//! crate is not a second runtime — it is a *provider profile* over the
//! generic gateway in `covenant-x402`:
//!
//! - [`catalog`] filters the orbit-x402 registry ([`covenant_x402::OrbitClient`])
//!   down to Xona's Solana-settled endpoints and keeps the endpoint
//!   index the tool layer reads. No daemon rebuild when Xona adds
//!   endpoints — the catalog is data, refreshed from the live registry.
//! - [`tools`] generates one [`covenant_mcp::Tool`] per endpoint, wired
//!   to a [`PaidExecutor`] the daemon supplies. The daemon is the only
//!   signer; this crate never touches a funding key.
//! - [`x402`] runs the 402-then-pay loop for one resolved call. Xona
//!   serves the plain (self-paid) Solana x402 scheme — no sponsor
//!   `feePayer` — so the daemon's signer sidecar self-pays with
//!   `SolanaSigner`.
//!
//! Settlement, budget, and audit are not re-implemented here: a Xona
//! call runs through the daemon's existing x402 accounting path, so its
//! receipt rolls into the same Merkle batch and optional Synapse mirror
//! as every other paid call.

#![deny(unsafe_code)]

pub mod catalog;
pub mod config;
pub mod tools;
pub mod x402;

pub use catalog::{XonaCatalog, XonaEndpoint};
pub use config::XonaConfig;
pub use tools::{xona_specs, xona_tool, xona_tools, PaidExecutor, PaidRequest, PaidResponse};
pub use x402::{execute_paid, parse_challenge, PaidHttp};

/// Vendored snapshot of Xona's orbit-x402 registry entries, captured for
/// offline startup. The daemon refreshes from the live registry on boot;
/// this copy is the fallback so a fresh install has the catalog without a
/// network round-trip.
pub const VENDORED_SNAPSHOT: &str = include_str!("../assets/xona-orbit-snapshot.json");

#[derive(Debug, thiserror::Error)]
pub enum XonaError {
    #[error("snapshot: {0}")]
    Snapshot(String),
    #[error("402 challenge: {0}")]
    Challenge(String),
    #[error("no payment option matched policy: {0}")]
    NotAllowed(String),
    #[error("paid call: {0}")]
    Execute(String),
    #[error("registry: {0}")]
    Registry(String),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
}

pub type Result<T> = std::result::Result<T, XonaError>;
