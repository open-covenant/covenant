//! Covenant on Loofta Pay: safe agent payments, and provenance the payer can
//! stand behind.
//!
//! Loofta Pay sends private payments on Solana to a username, email, or social
//! handle, settling to a destination the sender never has to see. Covenant adds
//! two things on top of that rail, both here and both tested:
//!
//! 1. **A pre-send guard.** Before a payment is signed, run the recipient
//!    against its Covenant standing and return allow, refuse, or escalate
//!    ([`policy`], [`recipient`]). A payout to a flagged or sanctioned
//!    destination never leaves the wallet, and the decision is captured as a
//!    signed [`receipt`].
//! 2. **A privacy-preserving provenance record.** A settled payment folds a
//!    commitment into Covenant's provenance root and binds it to an attestation
//!    ([`provenance`]). Only a hash is anchored, so no amount or party is
//!    revealed; the payer opens the record later to prove the payment happened
//!    as recorded, without giving up the privacy the rail gives them.
//!
//! [`tools`] exposes the guard as a read-only MCP preview. Nothing here touches
//! Loofta's rail or moves funds.
//!
//! What it needs from Loofta to run live, and deliberately does not invent:
//! - where in their send flow the gate is called (they hand us the resolved
//!   recipient and amount before signing, or the gate sits in front of it),
//! - how a username / email / social handle resolves to the destination pubkey
//!   the gate scores,
//! - their API surface and auth, if the call is server to server.
//!
//! Two things stay honest here. Loofta's privacy layer (MagicBlock) unlinks
//! sender and recipient on-chain, so most destinations are fresh and carry no
//! Covenant history; the policy treats an unknown recipient as normal rather
//! than a red flag, and gates strangers on amount. And the enclave binding is
//! live substrate, not a claim to hedge: Loofta runs on MagicBlock Private
//! Ephemeral Rollups (Intel TDX), and Covenant already DCAP-verifies those
//! enclaves on mainnet, so [`provenance::SignedCommitment::enclave`] is filled
//! from a real verification. The lone gap is a `covenant-tee` crate that
//! packages the agent-bound challenge as a first-class attestation; the
//! mechanism itself is exercisable today.

#![deny(unsafe_code)]

pub mod config;
pub mod policy;
pub mod provenance;
pub mod receipt;
pub mod recipient;
pub mod tools;

mod error;
pub use error::{Error, Result};

pub use config::LooftaConfig;
pub use policy::{PayoutPolicy, Verdict};
pub use provenance::{EnclaveAttestation, PaymentRecord, SignedCommitment};
pub use receipt::{Decision, PayoutReceipt, SignedReceipt};
pub use recipient::{RecipientOracle, RecipientStanding, StaticOracle};
pub use tools::{loofta_tools, PAYOUT_CHECK_TOOL, PROVIDER, TRUST_LABEL};

pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
