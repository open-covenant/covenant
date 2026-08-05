//! Bonded operators: money at risk behind a promise, slashed from a record.
//!
//! An enclave proves code ran untampered. It does not prove the operator
//! behaved, and it cannot pay anyone back when they don't. Wherever a system
//! asks users to hand over funds and trust a service to act, the missing piece
//! is not more proof that the service exists. It is a number the service loses
//! when it fails.
//!
//! This is that mechanism, in three parts:
//!
//! - [`observation`] records what an outsider could see the operator do, hash
//!   chained so the record cannot be edited after the fact.
//! - [`sla`] turns that record and a promise into a verdict, as a pure function
//!   both sides can run.
//! - [`claim`] turns a breach into a slash bound to the record it came from,
//!   with no constructor that takes an amount and a reason.
//!
//! # What this can and cannot slash
//!
//! It slashes **liveness**: work taken on and not completed in time. Every
//! input is observable from outside, so an operator on a private rail can be
//! held to it without anyone seeing what was settled. An [`observation::Obligation`]
//! carries an opaque handle, a timestamp, and an outcome. Nothing else.
//!
//! It does not slash **correctness**: whether settlement went to the right
//! party for the right amount. That is not observable from outside a private
//! rail, and no amount of care in this crate changes that. Closing it needs a
//! wronged party to prove the operator misbehaved through selective disclosure,
//! without exposing anyone else. That is real design work and it is not here.
//! The distinction is kept in the type system rather than in a comment:
//! [`observation::Outcome`] has no variant for "wrong", so this crate cannot be
//! made to assert one.
//!
//! Being explicit about that split is the point. An SLA that quietly implied it
//! covered correctness would be worse than no SLA, because the number behind it
//! would be read as insurance against a failure it never watched.
//!
//! # Where the record goes
//!
//! The deployed settlement program folds receipts into an onchain provenance
//! root and reads a slash reason from that root through the seed-bound credit
//! account, so no caller supplies the reason. This crate produces the off-chain
//! half in the same shape: [`claim::SlashClaim`] carries the observation root
//! and re-derives its own verdict, so a claim that was edited anywhere stops
//! verifying. Submitting it is the keeper's step, not this crate's.

#![deny(unsafe_code)]

pub mod claim;
pub mod observation;
pub mod sla;

mod error;
pub use error::{Error, Result};

pub use claim::{Bond, SlashClaim, SlashPolicy, SCHEMA};
pub use observation::{Obligation, ObservationLog, Outcome};
pub use sla::{assess, Assessment, Sla};
