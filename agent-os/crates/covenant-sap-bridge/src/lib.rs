//! Bridge between Covenant and Synapse Agent Protocol (SAP v2).
//!
//! SAP is OOBE Protocol's on-chain identity, memory, reputation, and
//! commerce layer for AI agents on Solana. This crate is the
//! local-side adapter: it publishes the daemon's manifest as a SAP
//! agent account, resolves peer agents through the SAP discovery
//! registry, and publishes Covenant audit-root attestations into the
//! SAP attestation module — the public verification and
//! interoperability layer external parties read to confirm Covenant
//! roots.
//!
//! The bridge is strictly opt-in. Callers must pass a [`Config`] with
//! `enabled = true` for any on-chain path to fire. With the bridge
//! disabled the daemon must continue to operate fully offline — every
//! function here that touches the network gates on that flag.
//!
//! The daemon holds no JS runtime and no SAP SDK. Transaction
//! building, signing, and account decoding live in the TypeScript
//! bridge worker (`@covenant/sap-bridge`); this crate drives it as a
//! subprocess over a small JSON protocol (see [`worker`]).

#![deny(unsafe_code)]

pub mod attestation;
pub mod client;
pub mod config;
pub mod discovery;
pub mod identity;
pub mod worker;

pub use client::SapBridge;
pub use config::{Cluster, Config, DEFAULT_SYNAPSE_PROGRAM_ID};

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
    /// The bridge worker subprocess could not be spawned, failed to
    /// communicate, or produced no parseable output. Distinct from
    /// [`BridgeError::Rpc`], which is an error the worker itself
    /// reported after running.
    #[error("worker: {0}")]
    Worker(String),
}

pub type Result<T> = std::result::Result<T, BridgeError>;
