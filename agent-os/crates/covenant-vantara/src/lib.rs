//! Vantara provenance connector for Covenant.
//!
//! Vantara is a decentralized inference network: nodes run agent jobs, get
//! paid in USDC on Solana, and the network publishes a content-free
//! provenance record for every job — a job id, the node, the model, a
//! SHA-256 of the output, a signed receipt, a settlement flag, and a
//! completion time. Records carry only hashes and ids, never inputs,
//! outputs, or wallets.
//!
//! This crate reads that ledger and turns it into Covenant trust. It does
//! not run jobs and it never touches Vantara's payment rail. It fetches the
//! job feed ([`client`]), verifies each record's `vantaraSignature` against
//! the feed's self-describing signing block ([`verify`]), and mints a
//! content-free [`VantaraAttestation`] ([`attest`]) that binds an output hash
//! to the model and node that produced it, carrying the verification result
//! and whether the node reward settled on-chain. (`proofSignature` is a
//! network-internal MAC, not third-party verifiable, and is left opaque.)
//!
//! Everything is exposed as native [`covenant_mcp::Tool`]s ([`tools`]), so a
//! Vantara lookup is governed by the same capability checks and audit trail
//! as any other tool the daemon runs.
//!
//! [`VantaraAttestation`]: attest::VantaraAttestation

#![deny(unsafe_code)]

pub mod attest;
pub mod client;
pub mod config;
pub mod tools;
pub mod types;
pub mod verify;

pub use attest::{sha256_hex, VantaraAttestation, PROVIDER};
pub use client::{Lookup, VantaraClient};
pub use config::{VantaraConfig, BASE_URL};
pub use tools::{vantara_tools, ATTEST_TOOL, JOBS_TOOL, PAYOUTS_TOOL};
pub use types::{
    AssetTotal, Job, JobsPage, Pagination, Payout, PayoutsPage, Settlement, SigningBlock,
};
pub use verify::{
    canonical_receipt, check_output_hash, verify_ed25519, verify_receipt, HashCheck,
    SignatureStatus, RECEIPT_PREFIX,
};

#[derive(Debug, thiserror::Error)]
pub enum VantaraError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("explorer returned status {0}")]
    UnexpectedStatus(u16),
    #[error("decode: {0}")]
    Decode(String),
}

pub type Result<T> = std::result::Result<T, VantaraError>;
