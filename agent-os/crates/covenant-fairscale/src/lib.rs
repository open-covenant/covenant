//! FairScale reputation/credit provider for Covenant.
//!
//! FairScale (fairscale.xyz) scores Solana wallets and agents over three REST
//! products: reputation (`fairscore`), agent trust (a 0-100 score plus an
//! allow/deny `trust-gate`), and credit (an underwriting opinion). Covenant
//! consumes a FairScore as a **labeled soft signal**, the same posture as
//! [`covenant_krexa`]: a third-party value to weigh beside Covenant's own
//! audit-derived reputation, never blended into it and never a gate.
//!
//! FairScale's score is off-chain and a plain read is unsigned, so the read
//! cannot be proven after the fact. Covenant closes that: every read carries a
//! [`provenance::ReadProvenance`] record binding the pubkey queried, the
//! endpoint, and a hash of the exact response, so the read itself is verifiable
//! even though FairScale's number is not.
//!
//! Phase 0 authenticates with a `fairkey` header (FairScale's documented free
//! tier). Paying per read over x402 on Solana is the next increment; the read
//! surface is identical either way.
//!
//! One honest caveat, stamped in the trust label: FairScale's agent
//! `work_history` pillar is partly derived from Covenant's own exported conduct
//! events, so consuming the composite back is a routing prior, not an
//! independent correctness check.

pub mod client;
pub mod config;
pub mod provenance;
pub mod tools;

pub use client::{CreditAssessment, FairScaleClient, FairScore, TrustGate};
pub use config::{FairScaleConfig, AGENT_BASE_URL, REPUTATION_BASE_URL};
pub use provenance::ReadProvenance;
pub use tools::{fairscale_tools, PROVIDER, SCORE_TOOL, TRUST_LABEL};

#[derive(Debug, thiserror::Error)]
pub enum FairScaleError {
    #[error("fairscale http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("fairscale api [{status}]: {message}")]
    Api { status: u16, message: String },
    #[error("decode: {0}")]
    Decode(String),
}

pub type Result<T> = std::result::Result<T, FairScaleError>;
