//! Bridge between Covenant and SAID Protocol on Solana.
//!
//! SAID program: `5dpw6KEQPn248pnkkaYyWfHwu2nfb3LUMbTucb6LaA8G` (mainnet),
//! `ESPreFucjVwtDmZbhtL3JLJ9VxCethNEYtosMQhkcurv` (devnet).
//!
//! Off-chain register, lookup, and cross-chain messaging hit
//! `api.saidprotocol.com` in-process over REST. The four on-chain
//! instructions (`register_agent`, `get_verified`, `submit_anchor`,
//! `validate_work`) shell out to the TypeScript worker at
//! `@covenant/said-bridge` and each one is gated behind its own
//! `COVENANT_SAID_ALLOW_PAID_*` flag. All gates default off.

#![deny(unsafe_code)]

pub mod agent_card;
pub mod anchor;
pub mod client;
pub mod config;
pub mod cursor;
pub mod instructions;
pub mod rest;
pub mod worker;
pub mod xchain;

pub use client::SaidBridge;
pub use config::{Cluster, Config, PaidGates, DEFAULT_SAID_MAINNET_PROGRAM_ID, DEFAULT_SAID_DEVNET_PROGRAM_ID, DEFAULT_SAID_API_BASE_URL};

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("said bridge is disabled")]
    Disabled,
    #[error("paid instruction {instruction} is gated off")]
    PaidGateClosed { instruction: &'static str },
    #[error("rest: {0}")]
    Rest(String),
    #[error("http {status}: {body}")]
    Http { status: u16, body: String },
    #[error("{name}: {message}")]
    Upstream { name: String, message: String },
    #[error("decode: {0}")]
    Decode(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("worker: {0}")]
    Worker(String),
    #[error("worker timed out after {secs}s")]
    Timeout { secs: u64 },
}

pub type Result<T> = std::result::Result<T, BridgeError>;
