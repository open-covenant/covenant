//! Robinhood Crypto Trading API client, governed by Covenant.
//!
//! Phase 0 surface: Ed25519 request signing ([`signer`]), a read-only REST
//! client over a swappable [`transport`], typed order requests ([`types`]),
//! and the [`receipt`] black-box record. Order placement runs through the
//! daemon capability gate, and the on-chain anchor lands in later phases.
//!
//! Every request authenticates with `x-api-key` / `x-timestamp` /
//! `x-signature`, the last being Ed25519 over
//! `api_key + timestamp + path + method + body`.

pub mod client;
mod error;
pub mod governed;
pub mod policy;
pub mod receipt;
pub mod reputation;
pub mod signer;
pub mod transport;
pub mod types;

pub use client::RobinhoodClient;
pub use error::{Error, Result};
pub use governed::{
    Anchor, ApprovalGate, ApprovalOutcome, AutoApprove, AutoDeny, GovernedTrader, LocalAnchor,
};
pub use policy::{TradingPolicy, Verdict};
pub use receipt::{Decision, Mode, SignedReceipt, TradeReceipt};
pub use reputation::{trading_reputation, TradingReputation};
pub use signer::{RequestSigner, RobinhoodSigner, SignRequest, SignedHeaders};
pub use transport::{HttpRequest, HttpResponse, HttpTransport, MockTransport, Transport};
pub use types::{OrderRequest, OrderType, Side, TimeInForce};

/// Live base URL. The crypto paths under `/api/v1/crypto/...` are corroborated
/// across public sources; confirm against docs.robinhood.com before going live.
pub const DEFAULT_BASE_URL: &str = "https://trading.robinhood.com";

pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
