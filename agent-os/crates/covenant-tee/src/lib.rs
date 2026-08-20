//! Bind a TDX enclave quote to what actually ran inside it.
//!
//! Covenant already DCAP-verifies the MagicBlock ER enclaves on mainnet: the
//! ER verifier pulls a quote against a fresh challenge, checks it with the
//! Phala QVL against the Intel PCCS, and writes a verified-ER attestation to
//! the Solana Attestation Service. That answers "is this a genuine, current
//! enclave".
//!
//! It does not answer "did my thing run in it". The challenge is
//! `crypto.randomBytes(64)`, so the quote proves freshness and nothing else. A
//! quote taken for one payment is indistinguishable from a quote taken for
//! another, or for none.
//!
//! This crate closes that. [`Binding`] builds the 64 bytes of `report_data`
//! from an agent, a commitment to what ran, and a fresh nonce, so a verifier
//! holding those three can recompute the challenge and check the quote names
//! their subject and is fresh. A match names the triple rather than proving the
//! agent authorized it; [`Binding`] documents that boundary. [`BoundQuote`]
//! carries a DCAP result together with the binding it was taken under, and
//! [`BoundQuote::check`] reports the enclave half and the binding half
//! separately, since they fail for different reasons.
//!
//! The construction is sha512, which is exactly the 64 bytes `report_data`
//! holds, so a bound challenge drops into the existing
//! `GET /quote?challenge=<base64>` call with no change on the enclave side.
//! That is what makes this deployable against MagicBlock's rail as it stands
//! rather than a request for them to build something.
//!
//! Deliberately absent: fetching quotes, DCAP signature-chain verification, and
//! any judgement about which `mr_td` measurements are trustworthy. Those belong
//! to the verifier that already does them against the Intel PCCS. This crate
//! owns only the question that verifier structurally cannot answer.

#![deny(unsafe_code)]

pub mod attestation;
pub mod challenge;

mod error;
pub use error::{Error, Result};

pub use attestation::{BoundQuote, Check, SCHEMA, TCB_UP_TO_DATE};
pub use challenge::{Binding, DOMAIN, NONCE_LEN};
