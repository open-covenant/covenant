//! Krexa credit/risk provider for Covenant.
//!
//! Krexa fills the one primitive Covenant lacks: working capital for
//! agents (uncollateralized USDC credit, scored by the on-chain "Krexit
//! Score"). This crate consumes it in two layers with very different
//! risk:
//!
//! - **Read-only oracle (on by `enabled`).** Pure REST reads of an
//!   agent's Krexit score, eligibility, and active credit line, exposed
//!   as the `krexa.score` MCP tool and intended as a *labeled soft
//!   signal* fed alongside Covenant's own audit-derived reputation. No
//!   funds, no counterparty risk. The point: cross-reference "completes
//!   jobs" (Covenant) against "repays credit" (Krexa).
//!
//! - **Credit-backed x402 (gated by `credit_enabled`, OFF).** The seam
//!   for covering a 402 shortfall from a Krexa credit line and repaying
//!   from earnings. Built and tested behind the flag, never wired into
//!   the live x402 settlement path. See [`credit`] for the gating and
//!   the blockers (audit, vault TVL, custody model).
//!
//! Krexa's wider SDK (trade/swap/perps/pump.fun/KYA) is out of scope;
//! KYA in particular would collide with Covenant identity. Krexa is a
//! credit/score provider plugged into our identity, never an identity
//! source.
//!
//! ```no_run
//! use covenant_krexa::{KrexaClient, BASE_URL};
//!
//! # async fn demo() -> Result<(), covenant_krexa::KrexaError> {
//! let client = KrexaClient::new(BASE_URL);
//! let score = client.score("EnteGjokMnFqTDcZSBitXDQEctMCnqV33HbPKw2LnDCg").await?;
//! println!("krexit score {:?}, band {:?}", score.score(), score.risk_band());
//! # Ok(())
//! # }
//! ```

#![deny(unsafe_code)]

pub mod client;
pub mod config;
pub mod credit;
pub mod tools;

pub use client::{CreditEligibility, CreditLine, KrexaClient, KrexitScore};
pub use config::{KrexaConfig, BASE_URL};
pub use credit::{shortfall_draw_lamports, DrawRequest, SignedDraw};
pub use tools::{krexa_tools, PROVIDER, SCORE_TOOL, TRUST_LABEL};

/// Errors surfaced by the Krexa client.
#[derive(Debug, thiserror::Error)]
pub enum KrexaError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("krexa api [{status}]: {message}")]
    Api { status: u16, message: String },
    #[error("decode: {0}")]
    Decode(String),
    /// A credit-backed draw was attempted while `credit_enabled` is false.
    #[error("krexa credit is disabled (credit_enabled=false); this path is built but off")]
    CreditDisabled,
}

pub type Result<T> = std::result::Result<T, KrexaError>;
