//! Outbound x402 client for Covenant agents.
//!
//! x402 is an open protocol for paid HTTP: a server responds with
//! `402 Payment Required` and a JSON body describing accepted
//! payment options; the client signs one option and retries with an
//! `x-payment` header. This crate is the local-side adapter the
//! Covenant daemon uses to make those calls on behalf of an agent.
//!
//! The crate stays narrow on purpose. It does not hold funding keys,
//! does not enforce per-day or total budgets, and does not record
//! receipts — those concerns live in the daemon (covenant-budget,
//! covenant-settlement, covenant-audit). What this crate does:
//!
//! - Parse a 402 challenge into typed [`PaymentRequirements`].
//! - Match requirements against a [`Capability`] the daemon supplies.
//! - Hand the chosen requirement to a [`Signer`] for the actual
//!   payment construction.
//! - Retry once with the resulting `x-payment` header and surface
//!   the paid response back to the caller.
//!
//! Two real signers ship in-crate: [`EvmSigner`] (Base, EIP-3009
//! `TransferWithAuthorization` over EIP-712 — gasless, no RPC) is always
//! built; `SolanaSigner` (SPL transfer) is gated behind the `solana`
//! feature to keep the Solana dep tree opt-in. Both hold the funding key
//! the daemon custodies; [`MockSigner`] covers the client loop in tests.

#![deny(unsafe_code)]

pub mod client;
pub mod evm;
pub mod flow;
mod http;
pub mod orbit;
pub mod signer;
pub mod types;

#[cfg(feature = "solana")]
pub mod solana;

pub use client::{http_client, Client};
pub use evm::{
    extra_assets_from_env, parse_extra_assets, EvmSigner, EXTRA_ASSETS_ENV, USDC_BASE_MAINNET,
    USDC_BASE_SEPOLIA,
};
pub use flow::PaidRequest;
pub use orbit::{Catalog, OrbitClient, Pagination, RegistryEntry, RegistryResponse};
pub use signer::{MockSigner, Signer};
pub use types::{Capability, PaymentExtra, PaymentRequirements};

#[cfg(feature = "solana")]
pub mod payai;

#[cfg(feature = "solana")]
pub use payai::PayaiSolanaSigner;
#[cfg(feature = "solana")]
pub use solana::SolanaSigner;

/// Errors surfaced by the x402 client.
///
/// The surface is intentionally narrow so the daemon can map each
/// variant onto a stable audit-event kind without inspecting
/// upstream library errors.
#[derive(Debug, thiserror::Error)]
pub enum X402Error {
    /// The endpoint did not return 402 and did not return success.
    /// Holds the upstream status for diagnostics.
    #[error("unexpected status: {0}")]
    UnexpectedStatus(u16),
    /// The 402 body did not decode as an array of payment
    /// requirements.
    #[error("decode challenge: {0}")]
    DecodeChallenge(String),
    /// No payment requirement in the 402 response matched the
    /// supplied capability — wrong chain, wrong asset, or amount
    /// over the per-call cap.
    #[error("no payment requirement matched capability")]
    NoMatch,
    /// The signer failed to construct a payment payload.
    #[error("sign: {0}")]
    Sign(String),
    /// The orbit-x402 registry response did not decode as a services
    /// list, or exceeded the response-size cap.
    #[error("registry: {0}")]
    Registry(String),
    /// Underlying HTTP transport failure.
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
}

pub type Result<T> = std::result::Result<T, X402Error>;

/// The chain family a payment `network` string belongs to.
///
/// The daemon uses this to route a 402 challenge to the right signer
/// sidecar, so normalization deliberately mirrors what the signers
/// themselves accept: any `solana:<cluster>` string is Solana (the
/// `SolanaSigner` prefix check), and the EVM spellings this codebase
/// meets in the wild — `eip155:<id>`, `base:<id>`, `base`,
/// `base-mainnet`, `base-sepolia` — resolve to their chain id, so
/// `base`, `base-mainnet`, and `eip155:8453` land in one bucket
/// instead of splitting per spelling. Anything else is unrecognized
/// and the caller must fail closed rather than guess a chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkFamily {
    Solana,
    Evm(u64),
}

/// Classify a payment requirement's `network`, or `None` when no
/// signer this crate knows about could handle it.
pub fn network_family(network: &str) -> Option<NetworkFamily> {
    if network.starts_with("solana:") {
        return Some(NetworkFamily::Solana);
    }
    evm::chain_id_for_network(network)
        .ok()
        .map(NetworkFamily::Evm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_family_buckets_every_alias_by_chain_not_spelling() {
        assert_eq!(
            network_family("solana:mainnet"),
            Some(NetworkFamily::Solana)
        );
        assert_eq!(
            network_family("solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp"),
            Some(NetworkFamily::Solana)
        );
        for n in ["base", "base-mainnet", "eip155:8453", "base:8453"] {
            assert_eq!(network_family(n), Some(NetworkFamily::Evm(8453)), "{n}");
        }
        for n in ["base-sepolia", "eip155:84532", "base:84532"] {
            assert_eq!(network_family(n), Some(NetworkFamily::Evm(84532)), "{n}");
        }
    }

    #[test]
    fn network_family_leaves_unrecognized_networks_unclassified() {
        // "solana" without the colon is not the prefix the SolanaSigner
        // accepts, and case is significant on both sides — a router must
        // fail closed on these, not guess.
        for n in [
            "ethereum",
            "eip155:notanumber",
            "",
            "solana",
            "SOLANA:mainnet",
            "Base",
        ] {
            assert_eq!(network_family(n), None, "{n:?}");
        }
    }
}
