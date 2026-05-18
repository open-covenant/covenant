//! Bridge between Covenant and Synapse Agent Protocol (SAP v2).
//!
//! SAP is OOBE Protocol's on-chain identity, memory, reputation, and
//! commerce layer for AI agents on Solana. This crate is the
//! local-side adapter: it publishes the daemon's manifest as a SAP
//! agent account, resolves peer agents through the SAP discovery
//! registry, and mirrors Covenant audit-root attestations into the SAP
//! attestation module.
//!
//! The bridge is strictly opt-in. Callers must pass a [`Config`] with
//! `enabled = true` for any on-chain path to fire. With the bridge
//! disabled the daemon must continue to operate fully offline — every
//! function here that touches the network gates on that flag.
//!
//! Skeleton crate: types and traits only. RPC, transaction building,
//! and account decoding land in follow-up commits.

#![deny(unsafe_code)]

pub mod attestation;
pub mod client;
pub mod config;
pub mod discovery;
pub mod identity;

pub use config::{Config, Cluster};
pub use client::SapBridge;

/// Errors surfaced by the SAP bridge.
///
/// The bridge keeps its error surface narrow so the daemon can map
/// each variant onto a stable audit-event kind without inspecting
/// upstream library errors.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    /// The bridge is disabled in config. Callers should treat this as
    /// a soft no-op, not a failure — it is the default state.
    #[error("synapse bridge is disabled")]
    Disabled,
    /// Network or RPC layer failure.
    #[error("rpc: {0}")]
    Rpc(String),
    /// The on-chain account exists but did not decode against the
    /// expected SAP schema. Indicates a program upgrade or a wrong
    /// program ID.
    #[error("decode: {0}")]
    Decode(String),
    /// Caller-supplied input failed local validation before any RPC.
    #[error("invalid input: {0}")]
    Invalid(String),
    /// Underlying HTTP transport failure.
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
}

pub type Result<T> = std::result::Result<T, BridgeError>;
