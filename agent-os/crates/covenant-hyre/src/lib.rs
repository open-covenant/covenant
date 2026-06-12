//! Hyre as an x402 provider profile.
//!
//! Hyre publishes a DeFi-intelligence API as paid HTTP: each endpoint
//! answers a 402 challenge and settles in USDC through the PayAI
//! facilitator. This crate is not a second runtime — it is a
//! *provider profile* over the generic gateway in `covenant-x402`:
//!
//! - [`manifest`] parses Hyre's published OpenAPI (the `x-payment-info`
//!   extension on every path) into a flat endpoint list. No daemon
//!   rebuild when Hyre adds endpoints — the catalog is data.
//! - [`catalog`] turns that list into a [`covenant_x402::Catalog`], the
//!   same registry shape the gateway already drives, plus a richer
//!   endpoint index the tool layer reads for argument schemas.
//! - [`tools`] generates one [`covenant_mcp::Tool`] per endpoint and a
//!   single high-level `hyre.ask` tool, wired to a [`PaidExecutor`] the
//!   daemon supplies. The daemon is the only signer; this crate never
//!   touches a funding key.
//! - [`publish`] emits per-tool resale descriptors for the SAP bridge,
//!   a no-op when the bridge is off.
//!
//! Settlement, budget, attestation, and audit are not re-implemented
//! here: a Hyre call runs through the daemon's existing x402 accounting
//! path, so its receipt rolls into the same Merkle batch and optional
//! Synapse mirror as every other paid call.

#![deny(unsafe_code)]

pub mod catalog;
pub mod config;
pub mod facilitator;
pub mod manifest;
pub mod publish;
pub mod tools;
pub mod x402;

pub use catalog::HyreCatalog;
pub use config::HyreConfig;
// `facilitator` is intentionally NOT re-exported from the crate root.
// It's a diagnostic surface for the `live_*` examples; the production
// path never calls it. Access via the explicit `covenant_hyre::facilitator::…`
// path keeps that visible at the call site.
pub use manifest::Endpoint;
pub use tools::{hyre_specs, hyre_tool, hyre_tools, PaidExecutor, PaidRequest, PaidResponse};
pub use x402::{execute_paid, http_client, parse_challenge, PaidHttp};

/// Default Hyre OpenAPI manifest, vendored for offline startup.
///
/// The daemon refreshes from the live URL on an interval; this copy is
/// the boot-time fallback so a fresh install has the full catalog
/// without a network round-trip.
pub const VENDORED_MANIFEST: &str = include_str!("../assets/hyre-openapi.json");

#[derive(Debug, thiserror::Error)]
pub enum HyreError {
    #[error("manifest: {0}")]
    Manifest(String),
    #[error("402 challenge: {0}")]
    Challenge(String),
    #[error("no payment option matched policy: {0}")]
    NotAllowed(String),
    #[error("paid call: {0}")]
    Execute(String),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
}

pub type Result<T> = std::result::Result<T, HyreError>;
