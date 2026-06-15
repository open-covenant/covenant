//! Metaplex profile for Covenant.
//!
//! Exposes the Solana digital-asset layer to Covenant agents as a set of
//! capability-gated `metaplex.*` MCP tools, split into two surfaces:
//!
//! - **reads** ([`das`]) run Digital Asset Standard (DAS) queries — no
//!   keys, no spend, nothing leaves the host but a JSON-RPC call.
//! - **writes** ([`request`], [`tools::MetaplexSigner`]) anchor Covenant
//!   audit roots / agent identity into MPL Core assets. The daemon never
//!   holds the minting key or a `solana-sdk` dependency: it hands a
//!   [`request::SignerRequest`] to the `covenant-metaplex-signer` sidecar,
//!   mirroring the x402 funding-key isolation.
//!
//! This crate is intentionally `solana-sdk`-free so it links into the
//! daemon. All on-chain instruction building lives in the sidecar.

#![deny(unsafe_code)]

pub mod config;
pub mod das;
pub mod request;
pub mod tools;

pub use config::MetaplexConfig;
pub use das::{DasClient, DasError, HttpDasClient};
pub use request::{
    validate_registration_uri, validate_root_hash_hex, AttestationPayload, AttestationSubject,
    CovenantRelease, SignerRequest, SignerResponse, ATTESTATION_HASH_ALG, ATTESTATION_SCHEMA,
    ATTESTATION_TYPE, SUBJECT_REGISTRY,
};
pub use tools::{metaplex_specs, metaplex_tool, MetaplexSigner};
