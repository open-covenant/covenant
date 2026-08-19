//! Routing gateway for a Solana verifiable open-model inference network.
//!
//! The gateway is the network's one public endpoint. Nodes never listen; they dial
//! *out* to the gateway over mutual TLS and park reverse connections in a pool.
//! Callers reach the gateway with an ordinary OpenAI client pointed at
//! `.../v1`. A request is matched to a node by model and a trust floor, one of that
//! node's pooled tunnels is popped, and the request is proxied straight down it to
//! the node's own OpenAI surface — closing the `OpenAI(base_url) → gateway → node →
//! engine` path without the node ever accepting an inbound connection.
//!
//! The trust floor is honest by construction: a node's class is derived from its
//! signed telemetry posture via [`covenant_inference_protocol::NodePosture::effective_class`],
//! which clamps to what the network can actually verify. Nothing here reads a
//! node-asserted class off the wire.

pub mod pool;
pub mod proxy;
pub mod receipts;
pub mod registry;
pub mod server;
pub mod settlement;
pub mod tunnel;
pub mod x402;
