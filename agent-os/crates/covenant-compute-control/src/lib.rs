//! Durable authenticated authority and provider bridge for Covenant Compute.
//!
//! Beta usage receipts are bounded accounting evidence derived from immutable
//! provider quotes and durable runtime boundaries. They are not independent
//! metering or on-chain settlement proofs.

#![deny(unsafe_code)]

mod auth;
mod provider;
mod server;
mod service;
mod store;
mod vast;
mod web;

pub use auth::{AuthConfigError, AuthRegistry, BetaCredential, Principal};
pub use provider::{
    JobClock, ProviderBackend, ProviderCancel, ProviderError, ProviderJob, ProviderLaunch,
    ProviderPoll,
};
pub use server::{serve, ServerConfig, StartupError};
pub use service::{ControlPlane, PlanRejection, RecoveryReport, ServiceError};
pub use store::{SqliteStore, StoreError};
pub use vast::{VastBackend, VastBackendConfigError};
pub use web::router;

#[cfg(test)]
mod tests;
