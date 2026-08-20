#![deny(unsafe_code)]
//! Circuit LLM integration for Covenant.
//!
//! Circuit exposes an OpenAI-compatible inference gateway and an on-chain data API,
//! both metered with x402 and settled in CIRC (a Token-2022 mint) on Solana. This crate
//! makes both first-class, paid Covenant tools: it runs the x402 pay-and-retry loop,
//! enforces capability scoping (per-call cap, treasury pin, endpoint allowlist, and a
//! cumulative budget) before any CIRC leaves the wallet, and settles through a
//! [`CircPayer`] so a Covenant-paid call lands in Circuit's treasury identically to a
//! first-party one — feeding their staking and operator rewards, untouched.
//!
//! The pay-loop and clients are runtime- and Solana-free; the actual on-chain CIRC
//! transfer lives behind [`CircPayer`], with the real Token-2022 settling payer supplied
//! by the daemon or the live example so a funding key never enters this library.

mod capability;
mod config;
mod data;
mod inference;
mod payer;
#[cfg(feature = "solana")]
mod payer_solana;
mod tools;
mod x402;

pub use capability::{CircuitCapability, SpendLedger};
pub use config::CircuitConfig;
pub use data::DataClient;
pub use inference::{ChatMessage, ChatParams, ChatResult, Inference, Usage};
pub use payer::{CircPayer, MockCircPayer};
#[cfg(feature = "solana")]
pub use payer_solana::SolanaCircPayer;
pub use tools::{
    circuit_tools, DATA_QUERY_TOOL, INFERENCE_TOOL, MARKET_OVERVIEW_TOOL, TOKEN_PRICE_TOOL,
};
pub use x402::{Paid, PaidRequest, PaymentQuote, X402};

/// Pinned Circuit network constants. CIRC is a Token-2022 SPL mint on Solana mainnet.
pub mod circ {
    /// CIRC mint. Token-2022, 6 decimals.
    pub const MINT: &str = "8fQgfsRnRkKSeNUhevT7wp8mhNvMSJdLn1fJi4oVpump";
    /// SPL Token-2022 program that owns the CIRC mint (not the classic Token program).
    pub const TOKEN_PROGRAM: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
    /// CIRC decimals — a `transfer_checked` must assert this.
    pub const DECIMALS: u8 = 6;

    /// Inference gateway base (OpenAI-compatible; already includes the `/v1` segment).
    pub const INFERENCE_BASE: &str = "https://inference.circuitllm.xyz/v1";
    /// Data API base.
    pub const DATA_BASE: &str = "https://api.circuitllm.xyz";
    /// Default Solana RPC the settling payer targets.
    pub const RPC_URL: &str = "https://api.mainnet-beta.solana.com";
    /// Model id the inference gateway accepts (`circuit` is the live alias for the 72B).
    pub const DEFAULT_MODEL: &str = "circuit";

    /// Header carrying the settled payment's transaction signature on the retry.
    pub const PAYMENT_HEADER: &str = "X-Payment-Signature";
    /// Header that bypasses payment for trusted, co-located callers.
    pub const INTERNAL_KEY_HEADER: &str = "X-Internal-Key";
}

/// Everything that can go wrong talking to Circuit. Capability rejections are their own
/// variants so a caller can tell "the endpoint wanted too much" from "the network broke".
#[derive(Debug, thiserror::Error)]
pub enum CircuitError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("unexpected status {status}: {body}")]
    UnexpectedStatus { status: u16, body: String },
    #[error("endpoint returned 402 without usable payment requirements")]
    NoPaymentRequirements,
    #[error("decode {0}")]
    Decode(String),
    #[error("bad request url {0}")]
    BadUrl(String),

    #[error("quoted {amount_raw} raw CIRC exceeds the per-call cap of {cap}")]
    SpendCap { amount_raw: u64, cap: u64 },
    #[error("402 recipient {0} is not in the allowed treasury set — refusing to pay")]
    RecipientNotAllowed(String),
    #[error("endpoint host {0} is not in the capability's allowed set — refusing to pay")]
    EndpointNotAllowed(String),
    #[error("paying {amount_raw} would exceed the budget ({spent}/{budget} raw CIRC spent)")]
    BudgetExceeded {
        amount_raw: u64,
        spent: u64,
        budget: u64,
    },

    #[error("payment failed: {0}")]
    Payment(String),
}

pub type Result<T> = std::result::Result<T, CircuitError>;
