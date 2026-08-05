//! ClawVille profile for Covenant — AI↔AI bounty verification.
//!
//! Covenant sits **alongside PayAI**, not in the money path. PayAI holds and
//! releases escrow; Covenant decides whether the work was actually done and
//! emits a verdict the release keys on. The three pieces map onto the three
//! things ClawVille asked for:
//!
//! - **bounty verification** → [`bounty::Verdict`] (the signed pass/fail)
//! - **action logging** → [`trail::AuditTrail`] (hash-chained evidence the
//!   verdict is grounded in; root anchored on-chain by the daemon)
//! - **capability-scoped skills** → [`bounty::BountyGrant`] (the worker acts
//!   only within the actions granted for that bounty; the verifier confirms
//!   it stayed in scope)
//!
//! [`land`] is the second surface: a pre-execution guard for programmable
//! land. Once a building can be wired to act, granting one capability must not
//! grant the parcel, so an action is refused unless the owner granted it, and
//! the irreversible ones cannot be granted at all.
//!
//! [`bounty::ReleaseDecision`] names which PayAI instruction fires and who
//! signs it (`release_payment` / buyer on pass, `refund_buyer` / admin on
//! fail) — it never moves funds. Exposed as capability-gated `clawville.*`
//! MCP tools ([`tools`]), gated like the Metaplex/Hyre profiles.

#![deny(unsafe_code)]

pub mod bounty;
pub mod config;
pub mod land;
pub mod tools;
pub mod trail;
pub mod validate;

pub use bounty::{
    verify, AcceptanceCriteria, BountyGrant, BountyOpened, Criterion, CriterionResult,
    ReleaseDecision, Submission, Verdict,
};
pub use config::ClawvilleConfig;
pub use land::{
    authorize, Decision, LandAction, LandGrant, LandPolicy, LandVerdict, DEFAULT_OWNER_RESERVED,
};
pub use tools::{clawville_specs, clawville_tool};
pub use trail::{ActionEntry, AuditTrail};
