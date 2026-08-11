//! Transport for an inference node: an outbound reverse-connection tunnel that a
//! node holds open to the gateway, plus the enrol/heartbeat daemon that keeps the
//! control plane's view of the node current.
//!
//! This layer is deliberately trust-model-agnostic. It moves bytes and signs the
//! identity/telemetry the protocol crate defines; it says nothing about how a
//! caller decides to trust a node. Attestation, trust classes and receipts live
//! in `covenant-inference-protocol` and the verification layer above, not here.

pub mod backoff;
pub mod daemon;
pub mod frame;
pub mod serve;
pub mod target;
pub mod tunnel;
