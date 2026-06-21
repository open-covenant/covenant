//! PayAI trust layer for Covenant.
//!
//! Covenant sits alongside PayAI's x402 payment rail and never touches the
//! money path. Reads PayAI's public on-chain settlements and turns them into
//! two attestations:
//!
//! - Phase 0: settlement-grounded reputation per agent wallet, signed by the
//!   Covenant identity (a verifiable credential).
//! - Phase 1: Covenant-signed work-receipts binding a settlement to the work
//!   delivered, with an optional buyer counter-signature.
//!
//! Does NOT move funds, hold escrow, or call verify/settle: read-after-
//! settlement plus attest. Solana is read over plain JSON-RPC via `reqwest`, so
//! the crate stays light and daemon-linkable (no `solana-sdk`).

#![deny(unsafe_code)]

mod config;
mod index;
mod oracle;
mod receipt;
mod reputation;
mod sign;
mod tool;
mod types;

pub use config::PayAiConfig;
pub use index::{parse_settlements, SettlementIndexer, SignatureInfo};
pub use oracle::signed_reputation_for;
pub use receipt::{countersign, issue_receipt, verify_countersignature, WorkReceipt};
pub use reputation::{compute_reputation, sign_reputation};
pub use sign::SignedEnvelope;
pub use tool::{payai_specs, payai_tool};
pub use types::{PayaiReputation, Settlement, Tier};

/// PayAI's Solana facilitator fee-payer. Co-signs/pays for every PayAI Solana
/// settlement, so watching it yields the complete settlement stream.
pub const PAYAI_SOLANA_FEE_PAYER: &str = "2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4";
/// USDC mint, Solana mainnet-beta.
pub const USDC_MAINNET_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
/// USDC mint, Solana devnet (Circle's official devnet mint).
pub const USDC_DEVNET_MINT: &str = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU";
/// SPL Token program, used to recognise transfer instructions in jsonParsed txs.
pub const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
/// The PayAI hosted facilitator (for reference / discovery reads).
pub const FACILITATOR_URL: &str = "https://facilitator.payai.network";
/// USDC decimals.
pub const USDC_DECIMALS: u32 = 6;

#[derive(Debug, thiserror::Error)]
pub enum PayaiError {
    #[error("rpc: {0}")]
    Rpc(String),
    #[error("parse: {0}")]
    Parse(String),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("identity: {0}")]
    Identity(#[from] covenant_identity::IdentityError),
    #[error("verify: {0}")]
    Verify(String),
    #[error("encoding: {0}")]
    Encoding(String),
}

pub type Result<T> = std::result::Result<T, PayaiError>;

/// True for either USDC mint we recognise.
pub fn is_usdc(mint: &str) -> bool {
    mint == USDC_MAINNET_MINT || mint == USDC_DEVNET_MINT
}
