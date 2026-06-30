//! Bento Guard provider for Covenant.
//!
//! Bento is a pre-execution firewall for agents: before a transaction is
//! signed it scores the intent (allow, block, or escalate) and records
//! strikes plus an on-chain reputation. Covenant consumes that without
//! rebuilding it, in two layers, both off by default:
//!
//! - **Screen (gated by `protect_enabled`).** A `bento.screen` tool that
//!   runs Bento's `protect()` over a natural-language intent and returns the
//!   verdict. The call goes through a Node sidecar running Bento's own SDK,
//!   not a reimplementation: their SDK does the MagicBlock encryption and
//!   keypair signing. The agent key never reaches the daemon; the sidecar
//!   reads it from a file path the daemon forwards (the path-not-value
//!   posture of the x402 signer sidecar). Every screen is bounded by a hard
//!   deadline, and on any guard failure the tool fails closed.
//!
//! - **Reputation (gated by `program_id`).** A `bento.reputation` tool that
//!   reads an agent's on-chain Bento standing (strikes, lock state) into a
//!   labeled soft signal, to weigh alongside Covenant's own audit-derived
//!   reputation, never blended into it. The on-chain decode is the one piece
//!   that needs Bento to publish its program id, PDA seeds, and account
//!   layout; until then the read returns [`BentoError::LayoutPending`].
//!
//! Lane discipline: Covenant never re-scores intent or rebuilds the firewall
//! (Bento owns that), and Bento never signs work-completion or issues
//! capability grants (Covenant owns that).

#![deny(unsafe_code)]

pub mod config;
pub mod guard;
pub mod reader;
pub mod reputation;
pub mod tools;
pub mod types;

pub use config::{BentoConfig, DEFAULT_GUARD_TIMEOUT_MS, DEFAULT_RPC_URL};
pub use guard::{
    gate_decision, is_strike, Guard, ScreenRequest, SubprocessGuard, STRIKE_RISK_THRESHOLD,
};
pub use reader::BentoReader;
pub use reputation::clean_signal;
pub use tools::{bento_tools, PROVIDER, REPUTATION_TOOL, SCREEN_TOOL};
pub use types::{AnalysisResult, BentoStanding, GateOutcome, ReputationSignal, Verdict};

/// Errors surfaced by the Bento provider.
#[derive(Debug, thiserror::Error)]
pub enum BentoError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    /// The guard sidecar failed to spawn, timed out, errored, or returned
    /// unusable output. Callers treat this as fail-closed (block the action).
    #[error("bento guard sidecar: {0}")]
    Sidecar(String),
    #[error("decode: {0}")]
    Decode(String),
    /// The agent has no Bento account on chain (never interacted with Bento).
    /// Distinct from the layout gate: the read worked, there is nothing to read.
    #[error("agent has no bento record on chain")]
    NotFound,
    /// On-chain reads are blocked until Bento publishes the program id, PDA
    /// seeds, and Action account layout. Everything up to the decode is built.
    #[error("bento on-chain layout not yet published (program id, seeds, account layout)")]
    LayoutPending,
}

pub type Result<T> = std::result::Result<T, BentoError>;
