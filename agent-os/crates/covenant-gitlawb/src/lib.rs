//! Opt-in bridge between Covenant agent identity and gitlawb.
//!
//! Maps a Covenant ed25519 signer to a `did:key`, signs HTTP requests per
//! RFC 9421 in the form gitlawb-node accepts, and builds ref-update certs.
//!
//! Disabled by default. With [`Config::enabled`] = `false` every network
//! call returns [`GitlawbError::Disabled`] without touching the wire.
//!
//! See <https://github.com/Gitlawb/node> for the gitlawb side of the
//! protocol.

#![deny(unsafe_code)]

pub mod cert;
pub mod client;
pub mod config;
pub mod did;
pub mod error;
pub mod http_sig;
pub mod provenance;

pub use cert::{RefUpdateCert, RefUpdateSignature};
pub use client::GitlawbClient;
pub use config::Config;
pub use did::{did_key_from_verifying_key, verifying_key_from_did_key};
pub use error::{GitlawbError, Result};
pub use http_sig::{sign_request, SignedHeaders};
pub use provenance::{into_ref_update_cert, CommitProvenance};
