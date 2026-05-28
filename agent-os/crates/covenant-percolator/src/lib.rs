//! Covenant-governed keeper agents for the Percolator perpetuals
//! protocol.
//!
//! Percolator (`aeyakovenko/percolator-prog`) deliberately pushes
//! liveness onto permissionless participants: users manage their own
//! oracle freshness, every asset has its own staleness slot, and the
//! crank + recovery flows are open for anyone to call. The unsolved
//! half of that design is **who** runs the keepers reliably without
//! being a trusted single operator. This crate is the accountability
//! layer that lets a swarm of independent operators do that work safely:
//!
//! - **capability-scoped** — each keeper carries an operator-signed
//!   [`KeeperScope`] pinning the market, the assets, and the verbs it
//!   may touch (push marks, crank, recover); anything else is dropped
//!   before any RPC.
//! - **budget-bounded** — every executed action atomically debits the
//!   agent's budget bucket. A hostile or buggy policy can't drain
//!   more than the operator granted.
//! - **verifiable** — every action is paired 1:1 with a Covenant
//!   settlement receipt (the budget debit id and the receipt id match),
//!   batched and anchored on Solana through the daemon's settlement
//!   pipeline. Behavior is auditable end-to-end.
//!
//! The default build is the trait, the in-memory mock, and the
//! governance loop — fully tested. The `solana` feature adds
//! byte-locked on-chain `Instruction` builders for the v16 keeper
//! surface (program tags 5 / 43 / 44 / 45 / 63), plus the synthetic
//! `KeeperAction` → `Instruction` bridge in [`onchain`].

#![deny(unsafe_code)]

pub mod capability;
pub mod client;
pub mod coordination;
pub mod keeper;
pub mod policy;
pub mod risk;
pub mod state;

#[cfg(feature = "solana")]
pub mod instruction;
#[cfg(feature = "solana")]
pub mod onchain;

pub use capability::{KeeperScope, ACTION_KEEPER};
pub use client::{ClientError, Execution, MockPercolator, PercolatorClient};
pub use keeper::{KeeperAgent, TickReport};
pub use policy::{KeeperPolicy, RecoveryPolicy};
pub use state::{
    ActionLabel, AssetIndex, AssetLifecycle, AssetState, KeeperAction, MarketState,
    PortfolioSnapshot,
};

/// Default percolator program id (from `percolator-cli.json`). Live
/// markets pin their own program id per deployment — operators set
/// the program they actually trust.
pub const DEFAULT_PROGRAM_ID: &str = "2SSnp35m7FQ7cRLNKGdW5UzjYFF6RBUNq7d3m5mqNByp";

#[derive(Debug, thiserror::Error)]
pub enum PercolatorError {
    #[error("capability scope: {0}")]
    Scope(String),
    #[error("client: {0}")]
    Client(String),
    #[error("budget: {0}")]
    Budget(String),
    #[error("settlement: {0}")]
    Settlement(String),
}

pub type Result<T> = std::result::Result<T, PercolatorError>;
