//! Pluggable provenance attachments for gitlawb ref-update certs.
//!
//! A `gitlawb/ref-update/v1` cert proves who pushed and where a ref moved to.
//! It does not say how the commit was produced. This crate adds an optional
//! `attestations` field that lets any provenance system (SLSA, Sigstore,
//! in-toto, agent runtimes) attach a typed, signed blob bound to the cert by
//! its hash.
//!
//! The cert format stays additive. A cert with no attestations serializes the
//! same bytes it does today, and an envelope with attestations deserializes
//! into existing `RefUpdateCert` code that ignores the extra field.
//!
//! ## Wire shape
//!
//! ```json
//! {
//!   "type": "gitlawb/ref-update/v1",
//!   // ...standard cert fields...,
//!   "signatures": [...],
//!   "attestations": [
//!     {
//!       "type": "covenant/exec/v1",
//!       "payload": { /* opaque, type-specific */ },
//!       "cert_hash": "<sha256 hex of JCS-encoded cert body>",
//!       "signer":    "did:key:z6Mk...",
//!       "sig":       "<base64url ed25519 over (type, payload, cert_hash)>"
//!     }
//!   ]
//! }
//! ```
//!
//! ## Verification
//!
//! [`Registry`] looks up a verifier by type discriminator. [`Policy`] decides
//! what to do when no verifier matches: `AcceptKnown` (default) lets unknown
//! types pass without trust, `RequireAll` enforces an allowlist per repo,
//! `RejectUnknown` rejects everything unregistered.
//!
//! ## Canonical bytes
//!
//! Hashing and signing inputs are encoded with JCS (RFC 8785) so they
//! reproduce across implementations regardless of struct field order or JSON
//! library.

#![deny(unsafe_code)]
#![deny(missing_docs)]

pub mod attestation;
pub mod cert;
pub mod error;
pub mod verifier;

pub use attestation::{Attestation, AttestationPayload};
pub use cert::{cert_hash, AttestedRefUpdateCert};
pub use error::{AttestError, Result};
pub use verifier::{AttestationVerifier, Policy, Registry, VerifiedAttestation};

/// Version string for the attestation envelope. Bumped if the binding rules
/// change.
pub const ATTEST_ENVELOPE_VERSION: &str = "v1";
