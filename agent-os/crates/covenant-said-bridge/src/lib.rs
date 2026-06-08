//! Bridge between Covenant and SAID Protocol (Solana Agent Identity Standard).
//!
//! SAID is the public agent-commons identity, reputation, and cross-chain
//! reach layer on Solana mainnet program `5dpw6KEQPn248pnkkaYyWfHwu2nfb3LUMbTucb6LaA8G`.
//! This crate is the local-side adapter: it registers a Covenant agent's
//! public identity, pushes Merkle-rooted audit slices into SAID's anchor
//! stream, emits `validate_work` records on completed FairScale-attested
//! jobs, and routes A2A messages over SAID's cross-chain hub.
//!
//! Plane separation (versus the existing Covenant settlement program at
//! `cov9UDyp…`): SAID is the public identity + reputation surface that
//! external platforms read across 10 chains. Covenant settlement is the
//! internal CVNT-economic credit-account + slash-vault. They share no
//! signer, no stake pool, and no slash authority.
//!
//! The bridge is strictly opt-in. Every paid on-chain operation is also
//! gated behind a per-instruction `COVENANT_SAID_ALLOW_PAID_*` flag so
//! an operator can fund anchor cadence without unlocking sponsorship.
//!
//! The daemon holds no JS runtime and no SAID SDK. Off-chain registration
//! and cross-chain messaging happen in-process over REST; the four paid
//! on-chain instructions (`register_agent`, `get_verified`,
//! `submit_anchor`, `validate_work`) are delegated to the TypeScript
//! bridge worker at `@covenant/said-bridge` over the same JSON envelope
//! contract used by `@covenant/sap-bridge`.

#![deny(unsafe_code)]

pub mod agent_card;
pub mod anchor;
pub mod client;
pub mod config;
pub mod cursor;
pub mod rest;
pub mod worker;

pub use client::SaidBridge;
pub use config::{Cluster, Config, PaidGates, DEFAULT_SAID_MAINNET_PROGRAM_ID, DEFAULT_SAID_DEVNET_PROGRAM_ID, DEFAULT_SAID_API_BASE_URL};

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("said bridge is disabled")]
    Disabled,
    #[error("paid instruction {instruction} is gated off")]
    PaidGateClosed { instruction: &'static str },
    #[error("rest: {0}")]
    Rest(String),
    #[error("http {status}: {body}")]
    Http { status: u16, body: String },
    #[error("{name}: {message}")]
    Upstream { name: String, message: String },
    #[error("decode: {0}")]
    Decode(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("worker: {0}")]
    Worker(String),
    #[error("worker timed out after {secs}s")]
    Timeout { secs: u64 },
}

pub type Result<T> = std::result::Result<T, BridgeError>;
