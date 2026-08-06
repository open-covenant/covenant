//! Covenant on Myrad: provenance for the step where verified contributions
//! become a signal someone buys.
//!
//! Myrad turns a person's own account activity into a privacy-safe behavior
//! signal an organization can buy. The source half of that is already proven:
//! Reclaim runs on the contributor's device, the raw data never leaves it, and
//! the proof says the activity is genuinely theirs. The half after it is where
//! the buyer is asked to take the pipeline's word: that the aggregate came from
//! N distinct consenting people, each backed by a valid proof, none counted
//! twice, inside the window it claims.
//!
//! This crate closes that half with one artifact, the
//! [`receipt::SignalReceipt`]. It binds the exact bytes delivered to a Merkle
//! root over the contributing set, records the contributor count and the time
//! range actually covered, and carries the result of every check in
//! [`integrity`] including the ones that came back soft. A buyer verifies it
//! offline against a pinned attestor key ([`tools`]). The digest is shaped for
//! the Solana anchoring path the daemon already runs for other receipt kinds;
//! this crate does not anchor, and [`SignedReceipt::anchor`] stays empty until
//! an issuer fills it.
//!
//! The design constraint that shapes everything here: Covenant never sees raw
//! data, an account identifier, or anything describing a person. Contributions
//! enter as provenance envelopes and salted pseudonyms Myrad computes on its
//! side, and every check runs on those. Adding this layer cannot weaken the
//! privacy claim Myrad already makes, because the layer has nothing to leak.
//!
//! What it does not do, and what that needs from Myrad:
//! - **Verify the Reclaim proof itself.** [`signal::ProofRef`] records which
//!   proof a contribution names and whether that name is a Reclaim proof id or
//!   an internal delivery marker, but the crate does not fetch or check the
//!   proof. Wiring that in needs a proof sample and Reclaim's verification
//!   surface, and until it exists the receipt says "as attested by the
//!   pipeline", not "as verified by Covenant".
//! - **Compute the aggregate.** Myrad's pipeline emits the signal; Covenant
//!   binds it. A receipt asserts the artifact came from the contributing set,
//!   never that the arithmetic over that set is right.
//! - **Mint the subject pseudonym.** `subject_commitment` is
//!   `sha256(secret_salt || "|" || user_id)` computed by Myrad. The salt stays on their
//!   side; a Covenant-side salt would mean Covenant holding user ids, which is
//!   the one thing this design refuses.

#![deny(unsafe_code)]

pub mod config;
pub mod evidence;
pub mod integrity;
pub mod receipt;
pub mod signal;
pub mod tools;

mod error;
pub use error::{Error, Result};

pub use config::MyradConfig;
pub use evidence::{Contribution, InclusionProof, MerkleTree, CONTRIBUTION_SCHEMA};
pub use integrity::{evaluate, Finding, IntegrityPolicy, IntegrityReport, Status};
pub use receipt::{Evidence, Freshness, SignalDescriptor, SignalReceipt, SignedReceipt};
pub use signal::{ProofRef, ProofRefKind, SignalRecord};
pub use tools::{myrad_tools, PROVIDER, TRUST_LABEL, VERIFY_SIGNAL_TOOL};

pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
