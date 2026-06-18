//! Zauth integration for Covenant.
//!
//! Two surfaces, both deliberately small:
//!
//! - [`directory`] is a read-only Rust client for
//!   `https://api.zauth.inc/api/directory`, the public, unauthenticated
//!   index of x402 endpoints zauth crawls. Tier-1 unauth gets ~30
//!   requests per window; honor `Retry-After` on 429.
//!
//! - [`reposcan`] runs the one-shot RepoScan call to
//!   `POST https://api.zauth.inc/x402/reposcan`, paid via the standard
//!   x402 challenge loop. Zauth uses mainline x402 v2 but carries the
//!   challenge in a `payment-required` response header (base64 JSON);
//!   the body is `{}`. This crate owns the header decoder so
//!   `covenant_x402::Client::request_paid` (which reads challenges off
//!   the body) does not need to grow header support.
//!
//! On Solana the challenge's `extra.feePayer` is the zauth treasury
//! ([`treasury::SOLANA`]), which fills the v0 message payer slot at
//! settle time. The injected signer partial-signs as the funder only.
//! On Base there is no `feePayer` and the caller funds gas.
//!
//! What this crate does NOT do: hold funding keys, enforce per-day or
//! total budgets, or expose zauth as an MCP-callable provider. RepoScan
//! is a one-shot operator-invoked CLI verb; budgeting lives at the CLI
//! layer.

#![deny(unsafe_code)]

pub mod challenge;
pub mod directory;
pub mod reposcan;

pub use directory::{
    DirectoryClient, DirectoryEndpoint, DirectoryPage, DirectoryStats, EndpointStatus, ListQuery,
    Pagination,
};
pub use reposcan::{RepoScanClient, RepoScanRequest, RepoScanResult};

#[derive(Debug, thiserror::Error)]
pub enum ZauthError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("unexpected status: {0}")]
    UnexpectedStatus(u16),
    #[error("decode directory: {0}")]
    DecodeDirectory(String),
    #[error("decode challenge: {0}")]
    DecodeChallenge(String),
    #[error("402 response missing payment-required header")]
    MissingChallengeHeader,
    #[error("no payment requirement matched: {0}")]
    NoMatch(String),
    #[error("payTo {got} does not match pinned zauth treasury {expected} for {network}")]
    UnpinnedPayTo {
        network: String,
        got: String,
        expected: String,
    },
    #[error("sign: {0}")]
    Sign(String),
}

pub type Result<T> = std::result::Result<T, ZauthError>;

/// Zauth treasury wallets confirmed live on 2026-06-03 via
/// `POST https://api.zauth.inc/x402/reposcan`.
pub mod treasury {
    /// Solana treasury. Receives RepoScan USDC and self-sponsors gas
    /// as the v0 message fee payer. Verified on-chain 2026-06-18 (17 SOL
    /// + 218 USDC, live RepoScan payee). NOTE: the June-3 probe
    /// transcribed this with an uppercase `B` (`…Nux8Bzvg…`), an empty
    /// address; the real treasury has a lowercase `b` (`…Nux8bzvg…`).
    pub const SOLANA: &str = "ZAU64eKWAgiGNux8bzvgRn8RvWqFhdMVrpJytF7V1qm";
    /// Base USDC treasury. Caller funds gas; no sponsored fee payer
    /// declared on the Base accept option.
    pub const BASE: &str = "0x2f5134f7cb98AF03099FA682555Ca1dD70D7D688";
}

pub mod assets {
    pub const USDC_SOLANA: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    pub const USDC_BASE: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
}

pub mod networks {
    pub const SOLANA_MAINNET: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";
    pub const BASE_MAINNET: &str = "eip155:8453";
}
