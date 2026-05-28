//! `covenant-percolator-bond` — the on-chain accountability layer
//! for permissionless accountable keepers.
//!
//! A keeper posts a SOL bond into a PDA derived from its identity
//! key. The bond carries a hash of the operator-signed `KeeperScope`
//! that authorized the keeper. **If the keeper executes any action
//! outside that scope**, anyone can submit signed evidence — the
//! offending settlement receipt + the canonical scope bytes that
//! hash matches what's stored — and the bond is atomically
//! transferred to a slash recipient (treasury / burner / operator).
//!
//! ## Why this matters
//!
//! Without a bond, the existing accountability surface (budget + audit
//! chain) is *post-hoc detection*: an operator sees that a keeper
//! misbehaved and can revoke its capability, but the misbehavior
//! already happened and there's no cost to it. The bond converts
//! detection into *deterrence*: a scope violation costs lamports
//! that the slasher (anyone watching the audit chain) collects.
//!
//! ## Design
//!
//! - **`BondAccount`** (PDA `[b"bond", keeper.as_ref()]`) — bytemuck-
//!   POD layout holding the keeper key, the operator key that signed
//!   the scope, the scope-hash, the bond lamports, a slashed flag,
//!   the slash recipient, and the bond's creation slot. Field order
//!   is locked by [`state::tests::layout_locked`].
//!
//! - **`SlashEvidence`** — the off-chain proof a slasher constructs.
//!   The verifier ([`evidence::verify_slash`]) is a pure function:
//!   inputs in → `Slashable` or `SlashRejection` out. It is the same
//!   logic the on-chain program runs (the program calls into this
//!   library; see [`program`]).
//!
//! - **`instruction`** — off-chain builders for `Deposit`, `Withdraw`,
//!   `Slash`. Wire format mirrors the percolator-prog convention
//!   (u8 tag + LE primitives).
//!
//! - **`program`** — the SBPF entrypoint, gated behind the `program`
//!   feature so the host build stays light. Off-feature, the same
//!   code paths run as plain Rust functions, fully testable.

#![deny(unsafe_code)]

pub mod evidence;
pub mod scope;
pub mod state;

#[cfg(feature = "solana")]
pub mod instruction;

// Program module hosts both the SBPF entrypoint (gated separately
// inside) and the pure-Rust handlers + tag constants the off-chain
// instruction builders reference. Always built; the SBPF
// entrypoint submodule is the only thing that needs the heavy
// `program` feature.
pub mod program;

pub use evidence::{verify_slash, SlashEvidence, SlashRejection, Slashable};
pub use scope::{ActionMask, BondScope, ScopeHash};
pub use state::BondAccount;

/// Seed prefix for the bond PDA: `[b"bond", keeper.as_ref()]`.
pub const BOND_SEED: &[u8] = b"bond";

/// Seed prefix for the slash-receipt PDA (replay protection):
/// `[b"slash", bond.as_ref(), receipt_id.as_ref()]`.
pub const SLASH_SEED: &[u8] = b"slash";

#[derive(Debug, thiserror::Error)]
pub enum BondError {
    #[error("scope hash mismatch")]
    ScopeHashMismatch,
    #[error("evidence does not establish a scope violation")]
    NotAViolation,
    #[error("receipt already used for slashing")]
    ReplayedReceipt,
    #[error("bond already slashed")]
    AlreadySlashed,
    #[error("bond insufficient: want {want}, have {have}")]
    Insufficient { want: u64, have: u64 },
    #[error("bond not paused (withdraw requires pause)")]
    NotPaused,
}
